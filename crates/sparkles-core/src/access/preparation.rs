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

struct Inner {
    control: Control,
    limit: u64,
    used: AtomicU64,
    peak: AtomicU64,
}

/// Shared cancellation, absolute deadline and memory accounting for preparing views.
/// Clones share one allowance. Reservations admit storage before it is allocated.
#[derive(Clone)]
pub struct Preparation {
    inner: Arc<Inner>,
    no_wait: bool,
}

impl Preparation {
    /// Start a request with a conservative allowance for its bookkeeping.
    pub fn new(control: Control, memory_bytes: u64) -> Result<Self> {
        control.check()?;
        if memory_bytes < CONTEXT_BYTES {
            return Err(memory(memory_bytes));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                control,
                limit: memory_bytes,
                used: AtomicU64::new(CONTEXT_BYTES),
                peak: AtomicU64::new(CONTEXT_BYTES),
            }),
            no_wait: false,
        })
    }

    /// Refuse a preparation lock that is already held instead of waiting for it.
    pub fn with_no_wait(mut self, no_wait: bool) -> Self {
        self.no_wait = no_wait;
        self
    }

    /// Poll this request's cancellation and absolute deadline.
    pub fn check(&self) -> Result<()> {
        self.inner.control.check()
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
        if !Arc::ptr_eq(&self.inner, &ownership.preparation.inner) {
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
        let mut workspace = self.reserve(0)?;
        if let Ok(id) = snap.generation.vocab.find_with_controlled(
            key.value.as_bytes(),
            |bytes| workspace.grow_to((bytes as u64).saturating_mul(2).saturating_add(256)),
            |phase| self.checkpoint(phase),
        )? {
            self.check()?;
            return Ok(Some(Id::vocab(id)));
        }
        let result = snap.generation.dvocab.with_prepared(self, |vocab| {
            Ok(vocab
                .find_prepared(key.value.as_bytes(), snap.dvocab_len, |_| self.check())?
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
        let mut used = self.inner.used.load(Ordering::Relaxed);
        loop {
            let next = used
                .checked_add(bytes)
                .filter(|&n| n <= self.inner.limit)
                .ok_or_else(|| memory(self.inner.limit))?;
            match self.inner.used.compare_exchange_weak(
                used,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.inner.peak.fetch_max(next, Ordering::Relaxed);
                    let held = Reservation {
                        preparation: self.clone(),
                        bytes,
                    };
                    self.check()?;
                    return Ok(held);
                }
                Err(actual) => used = actual,
            }
        }
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
        self.preparation
            .inner
            .used
            .fetch_sub(self.bytes, Ordering::Relaxed);
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
        if !Arc::ptr_eq(&self.strings.preparation.inner, &preparation.inner) {
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
pub struct PreparedView {
    access: Arc<GraphAccess>,
    snapshot: Arc<Snapshot>,
    _held: Reservation,
}

impl PreparedView {
    pub(crate) fn new(
        access: Arc<GraphAccess>,
        snapshot: Arc<Snapshot>,
        held: Reservation,
    ) -> Self {
        Self {
            access,
            snapshot,
            _held: held,
        }
    }

    /// Bound policy to apply while consuming the physical snapshot.
    pub fn access(&self) -> &GraphAccess {
        &self.access
    }

    /// Physical triple mask. Apply `access()` too; graph restrictions are not
    /// enforced by this raw snapshot alone.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let mut values = vec![9, 1, 8, 0, 1, 4, 7];
        let mut expected = values.clone();
        expected.sort_unstable();
        sort_by(&mut values, &preparation, |a, b| Ok(a.cmp(b))).unwrap();
        assert_eq!(values, expected);
    }
}
