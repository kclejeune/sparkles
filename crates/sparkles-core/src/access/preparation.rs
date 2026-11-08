//! Request-owned permission preparation. This context confers no authority.

use super::{GraphAccess, Graphs, Protection};
use crate::error::{Budget, BudgetKind, Error, Result};
use crate::id::{Id, Tag};
use crate::index::Perm;
use crate::store::Snapshot;
use crate::task::Control;
use oxrdf::{NamedNode, Term};
use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const CONTEXT_BYTES: u64 = 256;
// Conservative backing for the sealed parent, two boxed callbacks and retained
// shallow preparation handles. Admit before constructing those allocations.
const PARENT_BYTES: u64 = 512;

// No raw Arc/Weak conversion exists. Every clone uses this wrapper and every
// Drop consumes its Arc through into_inner, freeing the last Arc allocation
// before its returned payload/credit is destroyed (pinned Rust1.99.1 semantics).
struct Shared<T>(Option<Arc<T>>);
impl<T> Shared<T> {
    fn new(value: T) -> Self {
        Self(Some(Arc::new(value)))
    }
    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(
            self.0.as_ref().expect("live preparation"),
            other.0.as_ref().expect("live preparation"),
        )
    }
    #[cfg(test)]
    fn allocation_address(&self) -> usize {
        let counters = std::alloc::Layout::new::<[std::sync::atomic::AtomicUsize; 2]>();
        let (_, offset) = counters.extend(std::alloc::Layout::new::<T>()).unwrap();
        Arc::as_ptr(self.0.as_ref().expect("live preparation")) as usize - offset
    }
}
impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.as_deref().expect("live preparation")
    }
}
impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take().and_then(Arc::into_inner) {
            drop(value);
        }
    }
}

struct Inner {
    control: Control,
    limit: u64,
    used: AtomicU64,
    peak: AtomicU64,
    grant: Option<Arc<crate::sparql::pool::PoolGrant>>,
    context_bytes: u64,
    cancellation: Option<crate::task::Cancellation>,
    original_controls: Option<QueryControl>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(grant) = &self.grant {
            grant.release(self.context_bytes);
        }
    }
}

/// Shared cancellation, absolute deadline and memory accounting for preparing views.
/// Clones share one allowance. Reservations admit storage before it is allocated.
#[derive(Clone)]
pub struct Preparation {
    inner: Shared<Inner>,
    no_wait: bool,
    query: Option<QueryControl>,
}

// Inline shallow control handles add no allocation or independent allowance.
// Only checked source construction creates this view over a genuine root.
#[derive(Clone)]
struct QueryControl {
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    cancellation: Option<crate::task::Cancellation>,
    deadline: Option<std::time::Instant>,
}

impl Preparation {
    /// Admit one application's real request root before allocating derived work.
    /// The supplied options are the operation's actual policy and admission, not
    /// authority to read any source. Clones share this same allowance and controls.
    pub fn for_request(opts: &crate::sparql::QueryOptions) -> Result<Self> {
        Self::for_query_controls(opts, None, None, None)
    }

    /// Check a descendant's original controls before preparing its physical view.
    /// This shares the same root and actual grant. Relative timeouts must first be
    /// fixed to an absolute deadline, which the later source execution must reuse.
    /// A view cannot replace an already installed descendant control set.
    pub fn checked_for_query(&self, opts: &crate::sparql::QueryOptions) -> Result<Self> {
        self.check()?;
        if opts
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(Error::Cancelled);
        }
        if let Some(cancellation) = &opts.cancellation {
            cancellation.check()?;
        }
        let deadline = opts.effective_deadline(std::time::Instant::now())?;
        if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
            return Err(Error::Timeout);
        }
        if opts.timeout.is_some() && opts.deadline.is_none() {
            return Err(Error::invalid(
                "checked preparation requires an absolute query deadline",
            ));
        }
        crate::sparql::source_grant(self, opts)?;
        self.with_query_controls(opts.cancel.clone(), opts.cancellation.clone(), deadline)
    }

    /// Start a request with a conservative allowance for its bookkeeping.
    pub fn new(control: Control, memory_bytes: u64) -> Result<Self> {
        Self::new_with_grant(control, memory_bytes, None)
    }

    /// Prepare source ownership and descendant queries in one local allowance,
    /// backed by this genuine original pool grant. Later metadata cannot replace
    /// the grant that paid existing source payloads.
    pub fn with_grant(
        control: Control,
        memory_bytes: u64,
        grant: Arc<crate::sparql::pool::PoolGrant>,
    ) -> Result<Self> {
        Self::new_with_grant(control, memory_bytes, Some(grant))
    }

    fn new_with_grant(
        control: Control,
        memory_bytes: u64,
        grant: Option<Arc<crate::sparql::pool::PoolGrant>>,
    ) -> Result<Self> {
        control.check()?;
        if memory_bytes < CONTEXT_BYTES {
            return Err(memory(memory_bytes));
        }
        if let Some(grant) = &grant {
            grant.charge(CONTEXT_BYTES)?;
        }
        Ok(Self {
            inner: Shared::new(Inner {
                control,
                limit: memory_bytes,
                used: AtomicU64::new(CONTEXT_BYTES),
                peak: AtomicU64::new(CONTEXT_BYTES),
                grant,
                context_bytes: CONTEXT_BYTES,
                cancellation: None,
                original_controls: None,
            }),
            no_wait: false,
            query: None,
        })
    }

    /// Build an application operation's real shared root. Check and charge all
    /// fixed backing before allocating our own control, grant or Inner Arcs.
    pub(crate) fn for_query_controls(
        opts: &crate::sparql::QueryOptions,
        deadline: Option<std::time::Instant>,
        cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
        cancellation: Option<crate::task::Cancellation>,
    ) -> Result<Self> {
        if cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(Error::Cancelled);
        }
        if let Some(cancellation) = &cancellation {
            cancellation.check()?;
        }
        let deadline = deadline
            .into_iter()
            .chain(opts.effective_deadline(std::time::Instant::now())?)
            .min();
        if opts
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(Error::Cancelled);
        }
        if let Some(cancellation) = &opts.cancellation {
            cancellation.check()?;
        }
        if deadline.is_some_and(|end| std::time::Instant::now() >= end) {
            return Err(Error::Timeout);
        }
        // Includes Inner/control, the optional new grant, and alignment/Arc headers.
        let bytes = 1024u64;
        let layouts = [
            (std::mem::size_of::<Inner>(), std::mem::align_of::<Inner>()),
            (
                std::mem::size_of::<crate::sparql::pool::PoolGrant>(),
                std::mem::align_of::<crate::sparql::pool::PoolGrant>(),
            ),
            (
                std::mem::size_of::<std::sync::atomic::AtomicBool>(),
                std::mem::align_of::<std::sync::atomic::AtomicBool>(),
            ),
        ];
        let fixed = layouts
            .into_iter()
            .try_fold(0usize, |sum, (size, align)| {
                sum.checked_add(size)?
                    .checked_add(2 * std::mem::size_of::<usize>())?
                    .checked_add(2 * align)
            })
            .ok_or_else(|| Error::invalid("receiving root header size overflow"))?;
        if fixed as u64 > bytes {
            return Err(Error::invalid("receiving root header exceeds admission"));
        }
        let limit = opts.max_memory_bytes.unwrap_or(u64::MAX);
        if bytes > limit {
            return Err(Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                limit,
                requested: bytes,
            }));
        }
        let grant = if let Some(grant) = &opts.grant {
            grant.charge(bytes)?;
            Some(grant.clone())
        } else if let Some(pool) = &opts.pool {
            let grant = pool.empty_grant();
            grant.charge(bytes)?;
            Some(Arc::new(grant))
        } else {
            None
        };
        struct PartialHeader {
            grant: Option<Arc<crate::sparql::pool::PoolGrant>>,
            bytes: u64,
        }
        impl Drop for PartialHeader {
            fn drop(&mut self) {
                if let Some(grant) = &self.grant {
                    grant.release(self.bytes);
                }
            }
        }
        let mut partial = PartialHeader {
            grant: grant.clone(),
            bytes,
        };
        #[cfg(test)]
        {
            use crate::algo::memory::probe;
            probe::at(probe::Point::Allocation("output-root-preparation", 1));
            if probe::fails("output-root-preparation") {
                return Err(Error::invalid("injected receiving root allocation failure"));
            }
        }
        let mut control = match &opts.cancel {
            Some(flag) => Control::with_cancel(flag.clone()),
            None => Control::none(),
        };
        control.deadline = deadline;
        let root = Self {
            inner: Shared::new(Inner {
                control,
                limit,
                used: AtomicU64::new(bytes),
                peak: AtomicU64::new(bytes),
                grant,
                context_bytes: bytes,
                cancellation: opts.cancellation.clone(),
                original_controls: Some(QueryControl {
                    cancel,
                    cancellation,
                    deadline,
                }),
            }),
            no_wait: false,
            query: None,
        };
        partial.bytes = 0;
        root.check()?;
        Ok(root)
    }

    pub(crate) fn with_query_controls(
        &self,
        cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
        cancellation: Option<crate::task::Cancellation>,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self> {
        // Preparing a mask and executing that exact source reuse one descendant
        // control set. A different set must never replace the existing one.
        if let Some(original) = &self.query {
            let same_cancel = match (&original.cancel, &cancel) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            };
            let same_cancellation = match (&original.cancellation, &cancellation) {
                (Some(a), Some(b)) => a.same_source(b),
                (None, None) => true,
                _ => false,
            };
            if same_cancel && same_cancellation && original.deadline == deadline {
                self.check()?;
                return Ok(self.clone());
            }
            return Err(Error::invalid(
                "checked source already has descendant controls",
            ));
        }
        let mut view = self.clone();
        view.query = Some(QueryControl {
            cancel,
            cancellation,
            deadline,
        });
        view.check()?;
        Ok(view)
    }

    pub(crate) fn grant(&self) -> Option<&Arc<crate::sparql::pool::PoolGrant>> {
        self.inner.grant.as_ref()
    }

    /// One checked descendant shares the actual preparation root. The callbacks
    /// retain genuine ownership and original admission, not a domain label.
    pub fn memory_parent(&self) -> Result<sparkles_budget::ParentHandle> {
        let header = self.reserve(PARENT_BYTES)?;
        let take = self.clone();
        let give = self.clone();
        let domain = self
            .inner
            .grant
            .as_ref()
            .map_or(sparkles_budget::MemoryDomain::unpooled(), |grant| {
                grant.memory_domain()
            });
        let admitted = header.bytes();
        let parent = domain
            .parent_with_header(
                move |bytes| take.reserve_bytes(bytes),
                move |_: &sparkles_budget::ParentHandle, bytes| give.release_bytes(bytes),
                header,
                admitted,
            )
            .map_err(parent_error)?;
        self.check()?;
        Ok(parent)
    }

    fn reserve_bytes(&self, bytes: u64) -> std::result::Result<(), sparkles_budget::ParentRefused> {
        use sparkles_budget::{ParentRefused, Scope};
        let mut used = self.inner.used.load(Ordering::Relaxed);
        let next = loop {
            let next = used.checked_add(bytes).ok_or(ParentRefused::Overflow)?;
            if next > self.inner.limit {
                return Err(ParentRefused::Limit {
                    limit: self.inner.limit,
                    requested: next,
                    scope: Scope::Local,
                });
            }
            match self.inner.used.compare_exchange_weak(
                used,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break next,
                Err(actual) => used = actual,
            }
        };
        if let Some(grant) = &self.inner.grant
            && let Err(error) = grant.charge(bytes)
        {
            self.inner.used.fetch_sub(bytes, Ordering::Relaxed);
            let Error::BudgetExceeded(budget) = error else {
                unreachable!("grant charge only refuses memory")
            };
            return Err(ParentRefused::Limit {
                limit: budget.limit,
                requested: budget.requested,
                scope: Scope::Work,
            });
        }
        self.inner.peak.fetch_max(next, Ordering::Relaxed);
        Ok(())
    }

    fn release_bytes(&self, bytes: u64) {
        #[cfg(test)]
        crate::sparql::checked_source_tests::observe_release(self, bytes);
        self.inner.used.fetch_sub(bytes, Ordering::Relaxed);
        if let Some(grant) = &self.inner.grant {
            // No preparation lock is held when a grant invokes synchronous wakeups.
            grant.release(bytes);
        }
    }

    #[cfg(test)]
    pub(crate) fn inner_used(&self) -> u64 {
        self.inner.used.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn test_used_counter(&self) -> &AtomicU64 {
        &self.inner.used
    }

    /// The allowance not yet reserved. Other clones may reserve it at any moment, so
    /// this only sizes work; storage still needs a [`reserve`](Self::reserve).
    pub(crate) fn room(&self) -> u64 {
        self.inner
            .limit
            .saturating_sub(self.inner.used.load(Ordering::Relaxed))
    }

    // This is a sizing observation only. The actual reservation below the
    // selection boundary still decides atomic local/pool admission.
    pub(crate) fn selection_room(&self) -> u64 {
        let local = self.room();
        self.inner.grant.as_ref().map_or(local, |grant| {
            let covered = grant.granted().saturating_sub(grant.used());
            let free = grant
                .pool()
                .capacity()
                .saturating_sub(grant.pool().granted());
            local.min(covered.saturating_add(free))
        })
    }

    pub(crate) fn refuses_wait(&self) -> bool {
        self.no_wait
    }

    /// Refuse a preparation lock that is already held instead of waiting for it.
    pub fn with_no_wait(mut self, no_wait: bool) -> Self {
        self.no_wait = no_wait;
        self
    }

    /// Poll the complete original and descendant work controls in their original
    /// order. This is distinct from byte-only admission through an existing parent.
    pub fn check_work(&self) -> std::result::Result<(), sparkles_budget::WorkStopped> {
        use sparkles_budget::WorkStopped;
        self.inner
            .control
            .cancel
            .check()
            .map_err(|_| WorkStopped::Cancelled)?;
        if self
            .inner
            .control
            .deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            return Err(WorkStopped::Timeout);
        }
        if let Some(cancellation) = &self.inner.cancellation {
            cancellation.check().map_err(|_| WorkStopped::Cancelled)?;
        }
        for query in self.inner.original_controls.iter().chain(self.query.iter()) {
            if query
                .cancel
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                return Err(WorkStopped::Cancelled);
            }
            if let Some(cancellation) = &query.cancellation {
                cancellation.check().map_err(|_| WorkStopped::Cancelled)?;
            }
            if query
                .deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
            {
                return Err(WorkStopped::Timeout);
            }
        }
        Ok(())
    }

    /// Poll this request's cancellation and absolute deadline.
    pub fn check(&self) -> Result<()> {
        self.check_work().map_err(|stopped| match stopped {
            sparkles_budget::WorkStopped::Cancelled => Error::Cancelled,
            sparkles_budget::WorkStopped::Timeout => Error::Timeout,
        })
    }

    /// The cancellation, deadline and progress of this request.
    pub fn control(&self) -> &Control {
        &self.inner.control
    }

    /// Report a preparation phase and recheck controls after the callback returns.
    /// Providers must call this outside their internal locks.
    pub fn checkpoint(&self, phase: &str) -> Result<()> {
        self.stage(phase)
    }

    /// Compare policy text without an uninterruptible long string operation.
    pub fn compare(&self, a: &str, b: &str) -> Result<std::cmp::Ordering> {
        compare_text(a, b, self)
    }

    /// The existing `*` graph/predicate pattern semantics, with work checkpoints.
    pub fn matches(&self, pattern: &str, name: &str) -> Result<bool> {
        let (pattern, name) = (pattern.as_bytes(), name.as_bytes());
        let (mut i, mut j, mut work) = (0usize, 0usize, 0usize);
        let mut star = None;
        while j < name.len() {
            if work.is_multiple_of(1024) {
                self.check()?;
            }
            work = work.wrapping_add(1);
            if i < pattern.len() && pattern[i] == b'*' {
                star = Some((i, j));
                i += 1;
            } else if i < pattern.len() && pattern[i] == name[j] {
                i += 1;
                j += 1;
            } else if let Some((si, sj)) = star {
                i = si + 1;
                j = sj + 1;
                star = Some((si, sj + 1));
            } else {
                self.check()?;
                return Ok(false);
            }
        }
        for (index, &byte) in pattern[i..].iter().enumerate() {
            if index.is_multiple_of(1024) {
                self.check()?;
            }
            if byte != b'*' {
                return Ok(false);
            }
        }
        self.check()?;
        Ok(true)
    }

    fn exact(&self, values: &[String], value: &str) -> Result<bool> {
        let mut failure = None;
        let result = values.binary_search_by(|stored| {
            if failure.is_some() {
                return std::cmp::Ordering::Equal;
            }
            match self.compare(stored, value) {
                Ok(order) => order,
                Err(error) => {
                    failure = Some(error);
                    std::cmp::Ordering::Equal
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        self.check()?;
        Ok(result.is_ok())
    }

    /// Apply an already bound graph rule with its exact original ordering.
    pub fn allows_iri(&self, graphs: &Graphs, iri: &str) -> Result<bool> {
        self.check()?;
        let Graphs::Only(rule) = graphs else {
            return Ok(true);
        };
        if iri == crate::sparql::ctx::DEFAULT_GRAPH_IRI || iri == "default" {
            return Ok(rule.default_graph);
        }
        if iri == crate::sparql::ctx::UNION_GRAPH_IRI {
            return Ok(false);
        }
        self.named_rule(rule, iri)
    }

    fn named_rule(&self, rule: &super::GraphRule, iri: &str) -> Result<bool> {
        for pattern in &rule.excluded {
            if self.matches(pattern, iri)? {
                return Ok(false);
            }
        }
        for guard in &rule.guards {
            if self.matches(&guard.pattern, iri)?
                && (guard.decide.is_some() || !self.exact(&guard.exempt, iri)?)
            {
                return Ok(false);
            }
        }
        if self.exact(&rule.iris, iri)? {
            return Ok(true);
        }
        for protected in &rule.protected {
            if self.compare(protected, iri)?.is_eq() {
                return Ok(false);
            }
        }
        for pattern in &rule.patterns {
            if self.matches(pattern, iri)? {
                return Ok(true);
            }
        }
        self.check()?;
        Ok(false)
    }

    pub fn allows(&self, graphs: &Graphs, term: Option<&Term>) -> Result<bool> {
        self.check()?;
        match (graphs, term) {
            (Graphs::All, _) => Ok(true),
            (Graphs::Only(rule), None) => Ok(rule.default_graph),
            (Graphs::Only(rule), Some(Term::NamedNode(node))) => {
                if node.as_str() == crate::sparql::ctx::DEFAULT_GRAPH_IRI {
                    Ok(rule.default_graph)
                } else if node.as_str() == crate::sparql::ctx::UNION_GRAPH_IRI {
                    Ok(false)
                } else {
                    self.named_rule(rule, node.as_str())
                }
            }
            (Graphs::Only(rule), Some(Term::BlankNode(_))) => Ok(rule.blank_nodes),
            _ => Ok(false),
        }
    }

    pub(crate) fn covers_predicate(&self, protection: &Protection, iri: &str) -> Result<bool> {
        let Some(patterns) = &protection.predicates else {
            self.check()?;
            return Ok(true);
        };
        for pattern in patterns {
            if self.compare(pattern, iri)?.is_eq() || self.matches(pattern, iri)? {
                return Ok(true);
            }
        }
        self.check()?;
        Ok(false)
    }

    pub(crate) fn covers_graph(
        &self,
        protection: &Protection,
        graph: Option<&Term>,
    ) -> Result<bool> {
        match &protection.graphs {
            Some(graphs) => self.allows(graphs, graph),
            None => {
                self.check()?;
                Ok(true)
            }
        }
    }

    pub(crate) fn has_star(&self, pattern: &str) -> Result<bool> {
        for (index, &byte) in pattern.as_bytes().iter().enumerate() {
            if index.is_multiple_of(1024) {
                self.check()?;
            }
            if byte == b'*' {
                return Ok(true);
            }
        }
        self.check()?;
        Ok(false)
    }

    /// Copy provider-owned text under an ownership reservation from this request.
    pub fn copy_string(&self, text: &str, ownership: &mut Reservation) -> Result<String> {
        if !self.inner.same(&ownership.preparation.inner) {
            return Err(Error::Unsupported(
                "provider storage belongs to another preparation".into(),
            ));
        }
        ownership.merge(self.reserve((text.len() as u64).saturating_add(32))?);
        let mut output = String::new();
        output
            .try_reserve_exact(text.len())
            .map_err(|_| self.exhausted())?;
        copy_str(&mut output, text, self)?;
        Ok(output)
    }

    /// Stream exact physical quads without materializing an enumeration. The
    /// callback must reserve any owned output it retains through this context.
    pub fn scan(
        &self,
        snap: &Snapshot,
        perm: Perm,
        prefix: &[u64],
        each: impl FnMut([Id; 4]) -> Result<bool>,
    ) -> Result<()> {
        snap.scan_prepared(perm, prefix, self, each)
    }

    /// Resolve an IRI with controlled reconstruction and delta-vocabulary waiting.
    pub fn lookup_iri(&self, snap: &Snapshot, iri: &str) -> Result<Option<Id>> {
        self.check()?;
        let mut key = Text::new(self)?;
        key.push_str("<")?;
        key.push_str(iri)?;
        self.lookup_vocab_key(snap, key.value.as_bytes())
    }

    /// Resolve a ground term, an IRI, literal or triple term without blank nodes, with
    /// the controls of [`lookup_iri`](Self::lookup_iri). A blank node or a triple term
    /// containing one is refused, because a caller's blank-node label names no stored
    /// node.
    pub fn lookup_term(&self, snap: &Snapshot, term: &Term) -> Result<Option<Id>> {
        self.check()?;
        if !ground(term) {
            return Err(Error::invalid(
                "a blank node label does not identify a stored term",
            ));
        }
        if let Some(id) = crate::id::inline_id(term) {
            return Ok(Some(id));
        }
        if let Term::NamedNode(n) = term {
            return self.lookup_iri(snap, n.as_str());
        }
        let _held = self.reserve(term_bytes(term).saturating_add(64))?;
        let key = crate::id::term_key(term);
        self.lookup_vocab_key(snap, &key)
    }

    fn lookup_vocab_key(&self, snap: &Snapshot, key: &[u8]) -> Result<Option<Id>> {
        let mut workspace = self.reserve(0)?;
        if let Ok(id) = snap.generation.vocab.find_with_controlled(
            key,
            |bytes| workspace.grow_to((bytes as u64).saturating_mul(2).saturating_add(256)),
            |phase| self.checkpoint(phase),
        )? {
            self.check()?;
            return Ok(Some(Id::vocab(id)));
        }
        let result = snap.generation.dvocab.with_prepared(self, |vocab| {
            Ok(vocab
                .find_prepared(key, snap.dvocab_len, |_| self.check())?
                .map(Id::delta))
        })?;
        self.check()?;
        Ok(result)
    }

    /// Borrow a decoded term for a controlled provider. The callback must reserve
    /// any copies it keeps. Vocabulary locks are released before the callback.
    pub fn with_term<R>(
        &self,
        snap: &Snapshot,
        id: Id,
        each: impl FnOnce(Option<&Term>) -> Result<R>,
    ) -> Result<R> {
        self.check()?;
        let mut workspace = self.reserve(0)?;
        let decoded = match id.tag() {
            Tag::Vocab => snap
                .generation
                .vocab
                .get_with_controlled(
                    id.payload(),
                    |bytes| workspace.grow_to((bytes as u64).saturating_mul(2).saturating_add(256)),
                    |phase| self.checkpoint(phase),
                    |key| self.decode_term(key),
                )?
                .transpose()?,
            Tag::Delta => snap.generation.dvocab.with_prepared(self, |vocab| {
                if id.payload() >= snap.dvocab_len {
                    return Ok(None);
                }
                vocab
                    .get(id.payload())
                    .map(|key| self.decode_term(key))
                    .transpose()
            })?,
            _ => {
                let held = self.reserve(1024)?;
                snap.term(id).map(|term| (term, held))
            }
        };
        let result = each(decoded.as_ref().map(|(term, _)| term))?;
        self.check()?;
        Ok(result)
    }

    /// The lexical form of a stored simple literal, borrowed for `each` without decoding
    /// it as a term. Only the key's own bytes and the reconstruction workspace are
    /// charged, so a long retained text can be read within an allowance that would
    /// not hold a decoded term of it. `None` for any other term.
    pub fn with_simple_literal<R>(
        &self,
        snap: &Snapshot,
        id: Id,
        each: impl FnOnce(Option<&str>) -> Result<R>,
    ) -> Result<R> {
        self.check()?;
        let mut each = Some(each);
        let mut call = |key: &[u8]| -> Result<R> {
            let each = each.take().expect("called once");
            let _held = self.reserve((key.len() as u64).saturating_add(64))?;
            // `"`, the lexical form, and the separator with no language or datatype
            // after it. UTF-8 text never holds the separator byte.
            let lexical = match key {
                [b'"', text @ .., crate::id::KEY_SEP] => std::str::from_utf8(text).ok(),
                _ => None,
            };
            each(lexical)
        };
        let mut workspace = self.reserve(0)?;
        let result = match id.tag() {
            Tag::Vocab => snap.generation.vocab.get_with_controlled(
                id.payload(),
                |bytes| workspace.grow_to((bytes as u64).saturating_mul(2).saturating_add(256)),
                |phase| self.checkpoint(phase),
                &mut call,
            )?,
            Tag::Delta => snap.generation.dvocab.with_prepared(self, |vocab| {
                if id.payload() >= snap.dvocab_len {
                    return Ok(None);
                }
                Ok(vocab.get(id.payload()).map(&mut call))
            })?,
            _ => None,
        };
        let result = match result {
            Some(result) => result?,
            None => (each.take().expect("not called"))(None)?,
        };
        self.check()?;
        Ok(result)
    }

    fn decode_term(&self, key: &[u8]) -> Result<(Term, Reservation)> {
        // Includes decoded strings/boxes. Explicit frame/value buffers below carry
        // separate reservations and avoid a recursive native-stack decoder.
        let held = self.reserve((key.len() as u64).saturating_mul(128).saturating_add(1024))?;
        enum Frame<'a> {
            Key(&'a [u8], usize),
            Triple,
        }
        let mut frames = Buffer::new(self)?;
        let mut values = Buffer::new(self)?;
        frames.push(Frame::Key(key, 0))?;
        while let Some(frame) = frames.pop() {
            self.check()?;
            let term = match frame {
                Frame::Triple => {
                    let object = values
                        .pop()
                        .ok_or_else(|| Error::Corrupt("short triple key".into()))?;
                    let predicate = values
                        .pop()
                        .ok_or_else(|| Error::Corrupt("short triple key".into()))?;
                    let subject = match values
                        .pop()
                        .ok_or_else(|| Error::Corrupt("short triple key".into()))?
                    {
                        Term::NamedNode(node) => oxrdf::NamedOrBlankNode::NamedNode(node),
                        Term::BlankNode(node) => oxrdf::NamedOrBlankNode::BlankNode(node),
                        _ => oxrdf::NamedOrBlankNode::BlankNode(oxrdf::BlankNode::new_unchecked(
                            "invalid",
                        )),
                    };
                    if let Term::NamedNode(predicate) = predicate {
                        Term::Triple(Box::new(oxrdf::Triple::new(subject, predicate, object)))
                    } else {
                        Term::NamedNode(NamedNode::new_unchecked("urn:x-sparkles:invalid"))
                    }
                }
                Frame::Key(key, depth) => match key.first() {
                    Some(b'(') => {
                        if depth >= crate::nesting::MAX_TRIPLE_TERMS {
                            return Err(Error::Corrupt(
                                "stored triple term exceeds supported nesting".into(),
                            ));
                        }
                        let mut at = 1usize;
                        let mut children = [&[][..]; 3];
                        for child in &mut children {
                            let mut length = 0u64;
                            let mut shift = 0u32;
                            loop {
                                self.check()?;
                                let byte = *key.get(at).ok_or_else(|| {
                                    Error::Corrupt("short triple key length".into())
                                })?;
                                at += 1;
                                if shift > 63 || shift == 63 && byte > 1 {
                                    return Err(Error::Corrupt(
                                        "triple key length overflow".into(),
                                    ));
                                }
                                length |= u64::from(byte & 127) << shift;
                                if byte & 128 == 0 {
                                    break;
                                }
                                shift += 7;
                            }
                            let end = usize::try_from(length)
                                .ok()
                                .and_then(|n| at.checked_add(n))
                                .ok_or_else(|| {
                                    Error::Corrupt("triple key length overflow".into())
                                })?;
                            *child = key.get(at..end).ok_or_else(|| {
                                Error::Corrupt("short triple key component".into())
                            })?;
                            at = end;
                        }
                        frames.push(Frame::Triple)?;
                        for child in children.into_iter().rev() {
                            frames.push(Frame::Key(child, depth + 1))?;
                        }
                        continue;
                    }
                    Some(b'<') => {
                        Term::NamedNode(NamedNode::new_unchecked(lossy(&key[1..], self)?))
                    }
                    Some(b'"') => {
                        let mut separator = key.len();
                        for (seen, index) in (0..key.len()).rev().enumerate() {
                            if seen.is_multiple_of(1024) {
                                self.check()?;
                            }
                            if key[index] == crate::id::KEY_SEP {
                                separator = index;
                                break;
                            }
                        }
                        let lexical = lossy(&key[1..separator], self)?;
                        let suffix = key.get(separator + 1..).unwrap_or(&[]);
                        let literal = match suffix.first() {
                            None => oxrdf::Literal::new_simple_literal(lexical),
                            Some(b'@') => {
                                let mut tag = lossy(&suffix[1..], self)?;
                                let direction = match tag.rsplit_once("--") {
                                    Some((_, "ltr")) => Some(oxrdf::BaseDirection::Ltr),
                                    Some((_, "rtl")) => Some(oxrdf::BaseDirection::Rtl),
                                    _ => None,
                                };
                                if let Some(direction) = direction {
                                    tag.truncate(tag.len() - 5);
                                    oxrdf::Literal::new_directional_language_tagged_literal_unchecked(lexical, tag, direction)
                                } else {
                                    oxrdf::Literal::new_language_tagged_literal_unchecked(
                                        lexical, tag,
                                    )
                                }
                            }
                            Some(_) => oxrdf::Literal::new_typed_literal(
                                lexical,
                                NamedNode::new_unchecked(lossy(&suffix[1..], self)?),
                            ),
                        };
                        Term::Literal(literal)
                    }
                    Some(b'_') if key.len() == 9 => crate::id::key_to_term(key),
                    _ => Term::NamedNode(NamedNode::new_unchecked(lossy(key, self)?)),
                },
            };
            values.push(term)?;
        }
        let term = values
            .pop()
            .ok_or_else(|| Error::Corrupt("empty decoded key".into()))?;
        self.check()?;
        Ok((term, held))
    }

    /// Reserve additional storage before allocating it. Dropping the reservation
    /// releases its allowance, after its owner has dropped the storage.
    pub fn reserve(&self, bytes: u64) -> Result<Reservation> {
        self.check()?;
        self.reserve_bytes(bytes).map_err(|error| match error {
            sparkles_budget::ParentRefused::Limit {
                scope: sparkles_budget::Scope::Local,
                ..
            }
            | sparkles_budget::ParentRefused::Overflow => memory(self.inner.limit),
            error => parent_error(error),
        })?;
        let held = Reservation {
            preparation: self.clone(),
            bytes,
        };
        self.check()?;
        Ok(held)
    }

    /// Wait for a provider's mutex under the same request controls.
    pub fn lock<'a, T>(&self, lock: &'a Mutex<T>) -> Result<MutexGuard<'a, T>> {
        loop {
            self.check()?;
            if let Some(held) = lock.try_lock() {
                self.check()?;
                return Ok(held);
            }
            if self.no_wait {
                return Err(Error::invalid(
                    "controlled view preparation would wait (no_wait)",
                ));
            }
            self.waiting()?;
            if let Some(held) = lock.try_lock_for(Duration::from_millis(10)) {
                self.check()?;
                return Ok(held);
            }
        }
    }

    pub(crate) fn read_lock<'a, T>(&self, lock: &'a RwLock<T>) -> Result<RwLockReadGuard<'a, T>> {
        loop {
            self.check()?;
            if let Some(held) = lock.try_read() {
                self.check()?;
                return Ok(held);
            }
            if self.no_wait {
                return Err(Error::invalid(
                    "controlled view preparation would wait (no_wait)",
                ));
            }
            self.waiting()?;
            if let Some(held) = lock.try_read_for(Duration::from_millis(10)) {
                self.check()?;
                return Ok(held);
            }
        }
    }

    fn waiting(&self) -> Result<()> {
        self.inner
            .control
            .progress
            .report(0.0, "waiting for view preparation");
        self.check()
    }

    /// A privacy-safe memory refusal for provider allocation failures or overflow.
    pub fn exhausted(&self) -> Error {
        memory(self.inner.limit)
    }

    pub(crate) fn history_options(&self) -> crate::history::HistoryOptions {
        crate::history::HistoryOptions {
            cancel: Some(self.inner.control.cancel.flag()),
            deadline: self.inner.control.deadline,
        }
    }

    pub(crate) fn sanitize(&self, error: Error) -> Error {
        match error {
            Error::BudgetExceeded(Budget {
                kind: BudgetKind::Memory,
                ..
            }) => self.exhausted(),
            error => error,
        }
    }

    pub(crate) fn stage(&self, stage: &str) -> Result<()> {
        self.check()?;
        self.inner.control.progress.report(0.0, stage);
        self.check()
    }
}

/// Whether `term` contains no blank node.
fn ground(term: &Term) -> bool {
    match term {
        Term::BlankNode(_) => false,
        Term::Triple(t) => {
            !matches!(t.subject, oxrdf::NamedOrBlankNode::BlankNode(_)) && ground(&t.object)
        }
        _ => true,
    }
}

/// An upper bound of the vocabulary key bytes of `term`.
fn term_bytes(term: &Term) -> u64 {
    match term {
        Term::NamedNode(n) => n.as_str().len() as u64 + 1,
        Term::BlankNode(_) => 9,
        Term::Literal(l) => {
            let suffix = l
                .language()
                .map_or(l.datatype().as_str().len(), |tag| tag.len() + 8);
            (l.value().len() + suffix) as u64 + 16
        }
        Term::Triple(t) => {
            let subject: Term = t.subject.clone().into();
            term_bytes(&subject) + t.predicate.as_str().len() as u64 + term_bytes(&t.object) + 40
        }
    }
}

// Same replacement semantics as String::from_utf8_lossy, with at most1024 input
// bytes examined by each UTF-8 operation. The caller has admitted output storage.
fn lossy(mut bytes: &[u8], preparation: &Preparation) -> Result<String> {
    let mut out = String::new();
    out.try_reserve_exact(bytes.len().saturating_mul(3))
        .map_err(|_| preparation.exhausted())?;
    while !bytes.is_empty() {
        preparation.check()?;
        let length = bytes.len().min(1024);
        match std::str::from_utf8(&bytes[..length]) {
            Ok(text) => {
                out.push_str(text);
                bytes = &bytes[length..];
            }
            Err(error) => {
                let valid = error.valid_up_to();
                out.push_str(std::str::from_utf8(&bytes[..valid]).expect("validated UTF-8 prefix"));
                bytes = &bytes[valid..];
                match error.error_len() {
                    Some(invalid) => {
                        out.push('\u{fffd}');
                        bytes = &bytes[invalid..];
                    }
                    None if length < bytes.len().saturating_add(valid) => {}
                    None => {
                        out.push('\u{fffd}');
                        break;
                    }
                }
            }
        }
    }
    Ok(out)
}

fn parent_error(error: sparkles_budget::ParentRefused) -> Error {
    match error {
        sparkles_budget::ParentRefused::Limit {
            limit,
            requested,
            scope,
        } => Error::BudgetExceeded(Budget {
            kind: if scope == sparkles_budget::Scope::Local {
                BudgetKind::Memory
            } else {
                BudgetKind::ServerMemory
            },
            limit,
            requested,
        }),
        error => Error::Component(Box::new(
            crate::error::ComponentError::new("preparation-memory", "ownership", error.to_string())
                .with_source(error),
        )),
    }
}

fn memory(limit: u64) -> Error {
    Error::BudgetExceeded(Budget {
        kind: BudgetKind::Memory,
        limit,
        requested: limit.saturating_add(1),
    })
}

/// A formatter whose old and replacement string allocations share the cap.
pub(crate) struct Text {
    pub(crate) value: String,
    held: Reservation,
    failure: Option<Error>,
}

impl Text {
    pub(crate) fn new(preparation: &Preparation) -> Result<Self> {
        Ok(Self {
            value: String::new(),
            held: preparation.reserve(0)?,
            failure: None,
        })
    }

    pub(crate) fn push_str(&mut self, text: &str) -> Result<()> {
        let preparation = &self.held.preparation;
        preparation.check()?;
        let needed = self
            .value
            .len()
            .checked_add(text.len())
            .ok_or_else(|| preparation.exhausted())?;
        if needed > self.value.capacity() {
            let capacity = self
                .value
                .capacity()
                .checked_mul(2)
                .map(|n| n.max(needed).max(32))
                .ok_or_else(|| preparation.exhausted())?;
            let held = preparation.reserve(capacity as u64)?;
            let mut value = String::new();
            value
                .try_reserve_exact(capacity)
                .map_err(|_| preparation.exhausted())?;
            if value.capacity() != capacity {
                return Err(preparation.exhausted());
            }
            copy_str(&mut value, &self.value, preparation)?;
            let old = std::mem::replace(&mut self.value, value);
            drop(old);
            self.held = held;
        }
        copy_str(&mut self.value, text, &self.held.preparation)
    }

    pub(crate) fn finish(self) -> Result<(String, Reservation)> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        Ok((self.value, self.held))
    }
}

impl std::fmt::Write for Text {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        if self.failure.is_some() {
            return Err(std::fmt::Error);
        }
        if let Err(error) = self.push_str(text) {
            self.failure = Some(error);
            return Err(std::fmt::Error);
        }
        Ok(())
    }
}

fn copy_str(out: &mut String, text: &str, preparation: &Preparation) -> Result<()> {
    let mut start = 0;
    while start < text.len() {
        preparation.check()?;
        let mut end = start.saturating_add(1024).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        out.push_str(&text[start..end]);
        start = end;
    }
    preparation.check()
}

/// Sort keys in pieces that the standard sort orders in well under a millisecond,
/// then merge the pieces through an admitted buffer, polling between pieces and every
/// 1024 merged keys. This is several times faster than [`sort_by`]'s heapsort and
/// keeps the same bounded time between checks.
pub(crate) fn sort_keys(values: &mut [crate::index::Key], preparation: &Preparation) -> Result<()> {
    const PIECE: usize = 1 << 14;
    preparation.check()?;
    if values.len() <= PIECE {
        values.sort_unstable();
        return preparation.check();
    }
    for piece in values.chunks_mut(PIECE) {
        preparation.check()?;
        piece.sort_unstable();
    }
    let _held = preparation.reserve(
        (values.len() as u64).saturating_mul(std::mem::size_of::<crate::index::Key>() as u64),
    )?;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(values.len())
        .map_err(|_| preparation.exhausted())?;
    for piece in values.chunks(PIECE) {
        preparation.check()?;
        buffer.extend_from_slice(piece);
    }
    // `buffer` holds the sorted pieces; each pass merges pairs into the other side
    let mut in_buffer = true;
    let mut width = PIECE;
    while width < values.len() {
        let (from, to): (&[crate::index::Key], &mut [crate::index::Key]) = if in_buffer {
            (&buffer, &mut *values)
        } else {
            (&*values, &mut buffer)
        };
        let mut start = 0;
        while start < from.len() {
            let middle = (start + width).min(from.len());
            let end = (start + 2 * width).min(from.len());
            let (mut a, mut b, mut out) = (start, middle, start);
            while out < end {
                if out.is_multiple_of(1024) {
                    preparation.check()?;
                }
                if b >= end || (a < middle && from[a] <= from[b]) {
                    to[out] = from[a];
                    a += 1;
                } else {
                    to[out] = from[b];
                    b += 1;
                }
                out += 1;
            }
            start = end;
        }
        in_buffer = !in_buffer;
        width *= 2;
    }
    if in_buffer {
        for (to, from) in values.chunks_mut(PIECE).zip(buffer.chunks(PIECE)) {
            preparation.check()?;
            to.copy_from_slice(from);
        }
    }
    preparation.check()
}

/// In-place heapsort can propagate a refusal during ordering, without unwinding or
/// leaving an uninterruptible library sort running after cancellation.
pub(crate) fn sort_by<T>(
    values: &mut [T],
    preparation: &Preparation,
    mut compare: impl FnMut(&T, &T) -> Result<std::cmp::Ordering>,
) -> Result<()> {
    fn sift<T>(
        a: &mut [T],
        mut root: usize,
        preparation: &Preparation,
        compare: &mut impl FnMut(&T, &T) -> Result<std::cmp::Ordering>,
    ) -> Result<()> {
        loop {
            preparation.check()?;
            let Some(mut child) = root
                .checked_mul(2)
                .and_then(|n| n.checked_add(1))
                .filter(|&n| n < a.len())
            else {
                break;
            };
            if child + 1 < a.len() && compare(&a[child], &a[child + 1])?.is_lt() {
                child += 1;
            }
            if !compare(&a[root], &a[child])?.is_lt() {
                break;
            }
            a.swap(root, child);
            root = child;
        }
        Ok(())
    }
    for root in (0..values.len() / 2).rev() {
        sift(values, root, preparation, &mut compare)?;
    }
    for end in (1..values.len()).rev() {
        preparation.check()?;
        values.swap(0, end);
        sift(&mut values[..end], 0, preparation, &mut compare)?;
    }
    preparation.check()
}

pub(crate) fn compare_text(
    a: &str,
    b: &str,
    preparation: &Preparation,
) -> Result<std::cmp::Ordering> {
    for (a, b) in a.as_bytes().chunks(1024).zip(b.as_bytes().chunks(1024)) {
        preparation.check()?;
        let order = a.cmp(b);
        if !order.is_eq() {
            return Ok(order);
        }
    }
    preparation.check()?;
    Ok(a.len().cmp(&b.len()))
}

/// Opaque ownership of part of a view preparation's allowance.
pub struct Reservation {
    preparation: Preparation,
    bytes: u64,
}

impl Reservation {
    pub(crate) fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    /// Admit more storage in an owner that already retains earlier allocations.
    pub fn grow_by(&mut self, bytes: u64) -> Result<()> {
        self.merge(self.preparation.reserve(bytes)?);
        Ok(())
    }
    /// Grow an owner's reservation before growing its storage. This never releases
    /// capacity while the owner may still retain it.
    pub fn grow_to(&mut self, bytes: u64) -> Result<()> {
        if bytes > self.bytes {
            self.merge(self.preparation.reserve(bytes - self.bytes)?);
        } else {
            self.preparation.check()?;
        }
        Ok(())
    }
    /// Release the allowance above `bytes` once the owner retains no more than that.
    pub(crate) fn release_to(&mut self, bytes: u64) {
        if bytes < self.bytes {
            self.preparation.release_bytes(self.bytes - bytes);
            self.bytes = bytes;
        }
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn merge(&mut self, mut other: Self) {
        assert!(Arc::ptr_eq(
            &self.preparation.inner,
            &other.preparation.inner
        ));
        self.bytes = self.bytes.checked_add(other.bytes).expect("admitted sum");
        other.bytes = 0;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.preparation.release_bytes(self.bytes);
    }
}

// Storage is declared first, so it drops before its reservation.
pub(crate) struct Buffer<T> {
    values: Vec<T>,
    held: Reservation,
}

impl<T> Buffer<T> {
    pub(crate) fn new(preparation: &Preparation) -> Result<Self> {
        Ok(Self {
            values: Vec::new(),
            held: preparation.reserve(0)?,
        })
    }

    pub(crate) fn push(&mut self, value: T) -> Result<()> {
        let preparation = self.held.preparation.clone();
        preparation.check()?;
        if self.values.len() == self.values.capacity() {
            let capacity = self
                .values
                .capacity()
                .checked_mul(2)
                .map(|n| n.max(4))
                .ok_or_else(|| preparation.exhausted())?;
            let bytes = capacity
                .checked_mul(std::mem::size_of::<T>())
                .and_then(|n| u64::try_from(n).ok())
                .ok_or_else(|| preparation.exhausted())?;
            let held = preparation.reserve(bytes)?;
            let mut values = Vec::new();
            values
                .try_reserve_exact(capacity)
                .map_err(|_| preparation.exhausted())?;
            if std::mem::size_of::<T>() != 0 && values.capacity() != capacity {
                return Err(preparation.exhausted());
            }
            for value in self.values.drain(..) {
                preparation.check()?;
                values.push(value);
            }
            let old = std::mem::replace(&mut self.values, values);
            drop(old);
            self.held = held;
        }
        self.values.push(value);
        Ok(())
    }

    pub(crate) fn into_parts(self) -> (Vec<T>, Reservation) {
        (self.values, self.held)
    }

    pub(crate) fn pop(&mut self) -> Option<T> {
        self.values.pop()
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.values.truncate(len);
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

pub(crate) struct Map<K, V> {
    values: rustc_hash::FxHashMap<K, V>,
    held: Reservation,
}

impl<K: Copy + Eq + std::hash::Hash, V: Copy> Map<K, V> {
    pub(crate) fn new(preparation: &Preparation) -> Result<Self> {
        Ok(Self {
            values: Default::default(),
            held: preparation.reserve(0)?,
        })
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Result<Option<V>> {
        let preparation = &self.held.preparation;
        preparation.check()?;
        if !self.values.contains_key(&key) && self.values.len() == self.values.capacity() {
            let capacity = self
                .values
                .capacity()
                .checked_mul(2)
                .map(|n| n.max(4))
                .ok_or_else(|| preparation.exhausted())?;
            // Hash-table rounding, control bytes, alignment and the temporary old
            // table coexist. This private map stores only small Copy keys/values.
            let buckets = capacity
                .checked_mul(2)
                .and_then(usize::checked_next_power_of_two)
                .ok_or_else(|| preparation.exhausted())?;
            let bytes = buckets
                .checked_mul(std::mem::size_of::<(K, V)>().saturating_add(16))
                .and_then(|n| n.checked_add(1024))
                .ok_or_else(|| preparation.exhausted())?;
            let held = preparation.reserve(bytes as u64)?;
            let mut values = rustc_hash::FxHashMap::default();
            values
                .try_reserve(capacity)
                .map_err(|_| preparation.exhausted())?;
            for (&key, &value) in &self.values {
                preparation.check()?;
                values.insert(key, value);
            }
            let old = std::mem::replace(&mut self.values, values);
            drop(old);
            self.held = held;
        }
        Ok(self.values.insert(key, value))
    }
}

impl<K, V> Deref for Map<K, V> {
    type Target = rustc_hash::FxHashMap<K, V>;
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}

/// Controlled provider output. Pushes reserve both string and container storage.
pub struct Exemptions {
    values: Buffer<String>,
    strings: Reservation,
}

impl Exemptions {
    pub fn new(preparation: &Preparation) -> Result<Self> {
        Ok(Self {
            values: Buffer::new(preparation)?,
            strings: preparation.reserve(0)?,
        })
    }

    pub fn push(&mut self, iri: &str) -> Result<()> {
        let preparation = &self.strings.preparation;
        let held = preparation.reserve((iri.len() as u64).saturating_add(32))?;
        let mut value = String::new();
        value
            .try_reserve_exact(iri.len())
            .map_err(|_| preparation.exhausted())?;
        for (index, ch) in iri.chars().enumerate() {
            if index.is_multiple_of(1024) {
                preparation.check()?;
            }
            value.push(ch);
        }
        self.values.push(value)?;
        self.strings.merge(held);
        Ok(())
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.values.iter().map(String::as_str)
    }

    pub(crate) fn into_parts(
        self,
        preparation: &Preparation,
    ) -> Result<(Vec<String>, Reservation)> {
        if !self.strings.preparation.inner.same(&preparation.inner) {
            return Err(Error::Unsupported(
                "guard provider returned exemptions from another preparation".into(),
            ));
        }
        let (values, mut held) = self.values.into_parts();
        held.merge(self.strings);
        Ok((values, held))
    }
}

/// An exact bound access policy and the physical snapshot it reads.
/// Its reservations live as long as this returned owner. Accessors borrow that
/// owner; independently cloning raw data requires the caller's own accounting.
///
/// ```compile_fail,E0308
/// use sparkles_core::access::PreparedView;
/// use sparkles_core::store::Snapshot;
/// use std::sync::Arc;
/// fn escape(view: &PreparedView) -> Arc<Snapshot> {
///     Arc::clone(view.snapshot())
/// }
/// ```
#[derive(Clone)]
pub struct PreparedView {
    inner: Shared<PreparedInner>,
}

// Payloads precede their final credit owner. Shallow handles keep the actual
// policy/mask/snapshot allocation and its original Preparation alive together.
struct PreparedInner {
    access: Arc<GraphAccess>,
    snapshot: Arc<Snapshot>,
    // Inline in this same admitted backing; no second carrier allocation. Its
    // physical selected source/original policy precede its selection credit.
    selected: Option<crate::store::bound_read_source::PreparedRead>,
    _held: Reservation,
}

impl PreparedView {
    /// The existing fixed header admission covers both actual Arc backings.
    /// This is reserved before GraphAccess or PreparedInner is allocated.
    pub(crate) fn header_bytes() -> Result<u64> {
        fn backing<T>() -> Result<u64> {
            let counters = std::alloc::Layout::new::<[std::sync::atomic::AtomicUsize; 2]>();
            let (layout, _) = counters
                .extend(std::alloc::Layout::new::<T>())
                .map_err(|_| Error::invalid("prepared backing layout overflow"))?;
            u64::try_from(layout.pad_to_align().size())
                .map_err(|_| Error::invalid("prepared backing size overflow"))
        }
        backing::<PreparedInner>()?
            .checked_add(backing::<GraphAccess>()?)
            .map(|bytes| bytes.max(1024))
            .ok_or_else(|| Error::invalid("prepared backing headers overflow"))
    }

    pub(crate) fn new(
        access: Arc<GraphAccess>,
        snapshot: Arc<Snapshot>,
        held: Reservation,
        preparation: &Preparation,
    ) -> Result<Self> {
        Self::new_selected(access, snapshot, held, preparation, None)
    }

    pub(crate) fn new_selected(
        access: Arc<GraphAccess>,
        snapshot: Arc<Snapshot>,
        held: Reservation,
        preparation: &Preparation,
        selected: Option<crate::store::bound_read_source::PreparedRead>,
    ) -> Result<Self> {
        // Couple all incoming payloads and their credits on the stack before
        // any fallible validation, so early exits obey the same drop order.
        let backing = PreparedInner {
            access,
            snapshot,
            selected,
            _held: held,
        };
        preparation.check()?;
        if !preparation.inner.same(&backing._held.preparation.inner) {
            return Err(Error::invalid(
                "prepared view belongs to another preparation",
            ));
        }
        if backing._held.bytes() < Self::header_bytes()? {
            return Err(Error::invalid(
                "prepared view lacks its admitted backing headers",
            ));
        }
        Ok(Self {
            inner: Shared::new(backing),
        })
    }

    /// Retain this genuine prepared policy and physical source as one opaque
    /// shallow owner. New result copies still need their receiving admission.
    pub fn read_source(&self) -> crate::store::BoundReadSource {
        crate::store::BoundReadSource::prepared(self.clone())
    }

    pub(crate) fn query_input(&self) -> (&Arc<Snapshot>, &Arc<GraphAccess>, &Preparation) {
        (
            &self.inner.snapshot,
            &self.inner.access,
            &self.inner._held.preparation,
        )
    }

    pub(crate) fn validate_instance(&self, store: &crate::store::Store) -> Result<()> {
        self.inner
            .selected
            .as_ref()
            .ok_or_else(|| Error::invalid("strict source requires a Store-minted physical origin"))?
            .validate_instance(store)
    }

    pub(crate) fn original_policy(&self) -> Option<&Arc<GraphAccess>> {
        self.inner
            .selected
            .as_ref()
            .map(|selected| selected.policy())
    }

    /// Bound policy to apply while consuming the physical snapshot.
    pub fn access(&self) -> &GraphAccess {
        &self.inner.access
    }

    /// Physical triple mask. Apply `access()` too; graph restrictions are not
    /// enforced by this raw snapshot alone. The opaque read-source factory is
    /// the ownership boundary for checked consumers retaining this preparation.
    pub fn snapshot(&self) -> &Snapshot {
        &self.inner.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_work_stop_keeps_same_byte_parent_after_descendant_cancellation() {
        use sparkles_budget::WorkStopped;
        let root_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let descendant = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let root = Preparation::new(Control::with_cancel(root_flag.clone()), 8192).unwrap();
        let child = root
            .checked_for_query(&crate::sparql::QueryOptions {
                cancel: Some(descendant.clone()),
                ..Default::default()
            })
            .unwrap();
        let parent = child.memory_parent().unwrap();
        let first = parent.reserve(4096).unwrap();
        descendant.store(true, Ordering::Relaxed);
        assert_eq!(child.check_work(), Err(WorkStopped::Cancelled));
        assert!(matches!(child.check(), Err(Error::Cancelled)));
        assert!(matches!(child.reserve(1), Err(Error::Cancelled)));
        // Byte-only finalization does not resume work or create a fresh root.
        let finalization = parent.reserve(1024).unwrap();
        assert_eq!(child.check_work(), Err(WorkStopped::Cancelled));
        assert!(parent.reserve(4096).is_err());
        assert_eq!(root.inner_used(), 256 + PARENT_BYTES + 4096 + 1024);
        drop(finalization);
        drop(first);
        drop(parent);
        assert_eq!(root.inner_used(), 256);
        root_flag.store(true, Ordering::Relaxed);
        assert_eq!(child.check_work(), Err(WorkStopped::Cancelled));
    }

    #[test]
    fn request_receiver_checked_views_reuse_real_parent_and_one_aggregate_allowance() {
        use crate::sparql::{QueryOptions, output::NativeBudget, pool::QueryPool};
        let pool = QueryPool::new(4 << 20);
        let grant = Arc::new(pool.empty_grant());
        let opts = QueryOptions {
            grant: Some(grant.clone()),
            max_memory_bytes: Some(128 << 10),
            deadline: Some(std::time::Instant::now() + Duration::from_secs(60)),
            ..Default::default()
        };
        let root = Preparation::for_request(&opts).unwrap();
        let child = root.checked_for_query(&opts).unwrap();
        let same = child.checked_for_query(&opts).unwrap();
        assert!(root.inner.same(&child.inner));
        assert!(child.inner.same(&same.inner));
        assert_eq!(grant.used(), 1024);
        let first = NativeBudget::from_preparation(&child)
            .unwrap()
            .text(&"x".repeat(64 << 10))
            .unwrap();
        let used = grant.used();
        assert!(
            matches!(NativeBudget::from_preparation(&same).unwrap().text(&"y".repeat(64 << 10)),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory && b.limit == 128 << 10)
        );
        assert_eq!(grant.used(), used);
        drop(root);
        drop(child);
        drop(same);
        assert!(grant.used() >= first.len() as u64);
        assert_eq!(first.len(), 64 << 10);
        drop(first);
        assert_eq!(grant.used(), 0);
        drop(opts);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn checked_preparation_preserves_original_and_child_control_identity_without_replacement() {
        use crate::sparql::{QueryOptions, pool::QueryPool};
        use std::sync::atomic::AtomicBool;
        for stopped in 0..4 {
            let flags: [Arc<AtomicBool>; 4] =
                std::array::from_fn(|_| Arc::new(AtomicBool::new(false)));
            let opts = QueryOptions {
                cancel: Some(flags[0].clone()),
                cancellation: Some(crate::task::Cancellation::from_flags([flags[1].clone()])),
                deadline: Some(std::time::Instant::now() + Duration::from_secs(60)),
                max_memory_bytes: Some(1 << 20),
                ..Default::default()
            };
            let root = Preparation::for_request(&opts).unwrap();
            let mut child_opts = opts.clone();
            child_opts.cancel = Some(flags[2].clone());
            child_opts.cancellation =
                Some(crate::task::Cancellation::from_flags([flags[3].clone()]));
            let child = root.checked_for_query(&child_opts).unwrap();
            assert!(child.checked_for_query(&child_opts).is_ok());
            let mut changed = child_opts.clone();
            changed.cancel = Some(Arc::new(AtomicBool::new(false)));
            assert!(matches!(
                child.checked_for_query(&changed),
                Err(Error::Invalid(_))
            ));
            changed = child_opts.clone();
            // Equal current flag values or an equivalent-looking composition are
            // not proof that this is the same original cancellation source.
            changed.cancellation = Some(crate::task::Cancellation::from_flags([flags[3].clone()]));
            assert!(matches!(
                child.checked_for_query(&changed),
                Err(Error::Invalid(_))
            ));
            flags[stopped].store(true, Ordering::SeqCst);
            assert!(matches!(child.check(), Err(Error::Cancelled)));
        }
        let pool = QueryPool::new(4 << 20);
        let grant = Arc::new(pool.empty_grant());
        let root_opts = QueryOptions {
            grant: Some(grant.clone()),
            max_memory_bytes: Some(1 << 20),
            ..Default::default()
        };
        let root = Preparation::for_request(&root_opts).unwrap();
        let before = grant.used();
        let foreign = Arc::new(pool.empty_grant());
        let mut wrong = root_opts.clone();
        wrong.grant = Some(foreign.clone());
        assert!(
            matches!(root.checked_for_query(&wrong), Err(Error::Component(e)) if e.code == "foreign-grant")
        );
        assert_eq!(grant.used(), before);
        assert_eq!(foreign.used(), 0);
        wrong.cancel = Some(Arc::new(AtomicBool::new(true)));
        assert!(matches!(
            root.checked_for_query(&wrong),
            Err(Error::Cancelled)
        ));
        assert_eq!(grant.used(), before);
        assert_eq!(foreign.used(), 0);
        let mut relative = root_opts.clone();
        relative.timeout = Some(Duration::from_secs(1));
        assert!(matches!(
            root.checked_for_query(&relative),
            Err(Error::Invalid(_))
        ));
        assert_eq!(grant.used(), before);
        drop(root);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn checked_preparation_absolute_deadline_and_no_wait_apply_before_view_work() {
        use crate::sparql::QueryOptions;
        let root = Preparation::for_request(&QueryOptions {
            max_memory_bytes: Some(1 << 20),
            ..Default::default()
        })
        .unwrap()
        .with_no_wait(true);
        let end = std::time::Instant::now() + Duration::from_secs(60);
        let opts = QueryOptions {
            deadline: Some(end),
            ..Default::default()
        };
        let child = root.checked_for_query(&opts).unwrap();
        assert_eq!(child.query.as_ref().unwrap().deadline, Some(end));
        let lock = Mutex::new(());
        let held = lock.lock();
        assert!(matches!(child.lock(&lock), Err(Error::Invalid(_))));
        drop(held);
        let expired = QueryOptions {
            deadline: Some(std::time::Instant::now() - Duration::from_secs(1)),
            ..Default::default()
        };
        assert!(matches!(
            root.checked_for_query(&expired),
            Err(Error::Timeout)
        ));
        let before = root.inner_used();
        assert!(matches!(
            child.checked_for_query(&expired),
            Err(Error::Timeout)
        ));
        assert_eq!(root.inner_used(), before);
    }

    #[test]
    fn checked_source_parent_release_wakes_outside_preparation_and_grant_locks() {
        use crate::sparql::pool::QueryPool;
        use std::sync::atomic::AtomicUsize;
        let pool = QueryPool::new(64 << 20);
        let grant = Arc::new(pool.empty_grant());
        let preparation =
            Preparation::with_grant(Control::none(), 128 << 20, grant.clone()).unwrap();
        let parent = preparation.memory_parent().unwrap();
        let held = parent.reserve(48 << 20).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let reentry = parent.clone();
        let waiter = pool.enter(
            32 << 20,
            Some(Arc::new(move || {
                drop(reentry.clone().reserve(1).unwrap());
                observed.fetch_add(1, Ordering::SeqCst);
            })),
        );
        drop(held);
        assert!(calls.load(Ordering::SeqCst) > 0);
        assert_eq!(preparation.inner_used(), CONTEXT_BYTES + PARENT_BYTES);
        drop(waiter);
        drop(parent);
        drop(preparation);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn checked_source_parent_shares_original_local_and_pool_ownership() {
        use crate::sparql::pool::QueryPool;
        let pool = QueryPool::new(64 << 20);
        let grant = Arc::new(pool.empty_grant());
        let preparation = Preparation::with_grant(Control::none(), 4096, grant.clone()).unwrap();
        assert_eq!(grant.used(), CONTEXT_BYTES);
        let source = preparation.reserve(1024).unwrap();
        let parent = preparation.memory_parent().unwrap();
        assert_eq!(parent.domain(), pool.domain());
        let baseline = preparation.inner_used();
        let output = parent.reserve(4096 - baseline).unwrap();
        assert_eq!((preparation.inner_used(), grant.used()), (4096, 4096));
        assert!(matches!(
            parent.reserve(1),
            Err(sparkles_budget::ParentRefused::Limit {
                scope: sparkles_budget::Scope::Local,
                requested: 4097,
                ..
            })
        ));
        drop(source);
        assert_eq!((preparation.inner_used(), grant.used()), (3072, 3072));
        drop(output);
        drop(parent);
        assert_eq!(
            (preparation.inner_used(), grant.used()),
            (CONTEXT_BYTES, CONTEXT_BYTES)
        );
        drop(preparation);
        assert_eq!(grant.used(), 0);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn checked_source_parent_refusal_rolls_back_without_changing_source_credit() {
        use crate::sparql::pool::QueryPool;
        let pool = QueryPool::new(64 << 20);
        let grant = Arc::new(pool.empty_grant());
        let preparation =
            Preparation::with_grant(Control::none(), u64::MAX, grant.clone()).unwrap();
        let parent = preparation.memory_parent().unwrap();
        let source = preparation.reserve(1024).unwrap();
        let baseline = (preparation.inner_used(), grant.used());
        assert!(matches!(
            parent.reserve(64 << 20),
            Err(sparkles_budget::ParentRefused::Limit {
                scope: sparkles_budget::Scope::Work,
                ..
            })
        ));
        assert_eq!((preparation.inner_used(), grant.used()), baseline);
        assert!(matches!(
            parent.reserve(u64::MAX),
            Err(sparkles_budget::ParentRefused::Overflow)
        ));
        assert_eq!((preparation.inner_used(), grant.used()), baseline);
        drop(source);
        drop(preparation);
        let credit = parent.reserve(17).unwrap();
        assert_eq!(grant.used(), CONTEXT_BYTES + PARENT_BYTES + 17);
        drop(credit);
        drop(parent);
        assert_eq!(grant.used(), 0);
    }

    #[test]
    fn bound_read_source_keeps_real_prepared_backing_and_credit_until_last_clone() {
        let store = crate::store::Store::in_memory(Default::default());
        let snapshot = store.snapshot();
        let snapshot_owner = Arc::downgrade(&snapshot);
        let preparation = Preparation::new(Control::none(), 1 << 20).unwrap();
        let before = preparation.inner_used();
        let view = GraphAccess::all()
            .masked_controlled(&snapshot, &preparation)
            .unwrap();
        let backing = crate::sparql::checked_source_tests::AllocationWatch::new(
            view.inner.allocation_address(),
            &preparation,
        );
        let charged = preparation.inner_used();
        assert!(charged >= before + PreparedView::header_bytes().unwrap());
        let pressure = preparation.reserve(preparation.room()).unwrap();
        let source = view.read_source();
        let last = source.clone();
        drop(store);
        drop(snapshot);
        drop(view);
        drop(source);
        assert_eq!(backing.observed().1, 0);
        assert!(snapshot_owner.upgrade().is_some());
        assert!(matches!(
            preparation.reserve(1),
            Err(Error::BudgetExceeded(_))
        ));
        drop(pressure);
        assert_eq!(preparation.inner_used(), charged);
        drop(last);
        assert_eq!(backing.observed(), (charged, 1));
        assert!(snapshot_owner.upgrade().is_none());
        assert_eq!(preparation.inner_used(), before);
    }

    #[test]
    fn prepared_backing_refuses_foreign_credit_and_cancellation_before_allocation() {
        let store = crate::store::Store::in_memory(Default::default());
        let snapshot = store.snapshot();
        drop(store);
        let access = Arc::new(GraphAccess::all());
        let expected = Preparation::new(Control::none(), 8192).unwrap();
        let foreign = Preparation::new(Control::none(), 8192).unwrap();
        let foreign_before = foreign.inner_used();
        let held = foreign
            .reserve(PreparedView::header_bytes().unwrap())
            .unwrap();
        assert!(PreparedView::new(access.clone(), snapshot.clone(), held, &expected).is_err());
        assert_eq!(foreign.inner_used(), foreign_before);
        assert_eq!(Arc::strong_count(&access), 1);
        assert_eq!(Arc::strong_count(&snapshot), 1);
        let control = Control::none();
        let cancelled = Preparation::new(control.clone(), 8192).unwrap();
        let before = cancelled.inner_used();
        let held = cancelled
            .reserve(PreparedView::header_bytes().unwrap())
            .unwrap();
        control.cancel.cancel();
        assert!(matches!(
            PreparedView::new(access.clone(), snapshot.clone(), held, &cancelled),
            Err(Error::Cancelled)
        ));
        assert_eq!(cancelled.inner_used(), before);
        assert_eq!(Arc::strong_count(&access), 1);
        assert_eq!(Arc::strong_count(&snapshot), 1);
    }

    #[test]
    fn prepared_backing_is_admitted_before_policy_or_source_ownership() {
        let store = crate::store::Store::in_memory(Default::default());
        let snapshot = store.snapshot();
        let limit = CONTEXT_BYTES + PreparedView::header_bytes().unwrap() - 1;
        let preparation = Preparation::new(Control::none(), limit).unwrap();
        let before = preparation.inner_used();
        let owners = Arc::strong_count(&snapshot);
        assert!(matches!(
            GraphAccess::all().masked_controlled(&snapshot, &preparation),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(preparation.inner_used(), before);
        assert_eq!(Arc::strong_count(&snapshot), owners);
    }

    #[test]
    fn temporary_growth_releases_and_live_owners_share_one_allowance() {
        let preparation = Preparation::new(Control::none(), 8192).unwrap();
        let baseline = preparation.inner.used.load(Ordering::Relaxed);
        {
            let mut values = Buffer::new(&preparation).unwrap();
            for value in 0..96u64 {
                values.push(value).unwrap();
            }
            assert!(preparation.inner.peak.load(Ordering::Relaxed) > 128 * 8);
            assert!(preparation.inner.used.load(Ordering::Relaxed) > baseline);
            let free = preparation.inner.limit - preparation.inner.used.load(Ordering::Relaxed);
            let held = preparation.reserve(free).unwrap();
            assert!(matches!(
                preparation.reserve(1),
                Err(Error::BudgetExceeded(Budget {
                    limit: 8192,
                    requested: 8193,
                    ..
                }))
            ));
            drop(held);
        }
        assert_eq!(preparation.inner.used.load(Ordering::Relaxed), baseline);
        let mut output = Exemptions::new(&preparation).unwrap();
        assert!(output.push(&"x".repeat(8192)).is_err());
        drop(output);
        assert_eq!(preparation.inner.used.load(Ordering::Relaxed), baseline);
    }

    #[test]
    fn controlled_ordering_and_lossy_utf8_match_standard_results() {
        let preparation = Preparation::new(Control::none(), 1 << 20).unwrap();
        for bytes in [
            b"text".as_slice(),
            b"\xffa\xfe",
            b"\xf0\x9f\x92",
            b"\xf0\x9f\x92\xa1",
        ] {
            assert_eq!(
                lossy(bytes, &preparation).unwrap(),
                String::from_utf8_lossy(bytes)
            );
        }
        let mut bytes = vec![b'a'; 1023];
        bytes.extend_from_slice("💡".as_bytes());
        bytes.extend_from_slice(&[0xff, 0xfe]);
        assert_eq!(
            lossy(&bytes, &preparation).unwrap(),
            String::from_utf8_lossy(&bytes)
        );
        for len in [0usize, 1, 5, (1 << 14) + 3, 3 << 14, (5 << 14) + 17] {
            let mut keys: Vec<crate::index::Key> = (0..len as u64)
                .map(|i| {
                    let x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                    [x % 97, x >> 7, i % 5, 0]
                })
                .collect();
            let mut expected = keys.clone();
            expected.sort_unstable();
            let roomy = Preparation::new(Control::none(), 64 << 20).unwrap();
            sort_keys(&mut keys, &roomy).unwrap();
            assert_eq!(keys, expected, "{len} keys");
        }
        let mut values = vec![9, 1, 8, 0, 1, 4, 7];
        let mut expected = values.clone();
        expected.sort_unstable();
        sort_by(&mut values, &preparation, |a, b| Ok(a.cmp(b))).unwrap();
        assert_eq!(values, expected);
    }
}
