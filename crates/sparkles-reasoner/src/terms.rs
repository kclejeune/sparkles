//! Term handling for the engine: classification and decoding of ids, plus a local
//! dictionary for terms that do not exist in the store yet (head constants, literals
//! computed by builtins, blank nodes from `makeTemp` / `makeSkolem`).
//!
//! The local dictionary is charged to the run's budget ([`Terms::set_budget`]): each
//! term's key twice (the map's and the table's copy), its map entry, the map's growth
//! and each segment of the table when it is first used, before they are allocated; and
//! the regular expressions it compiled, at an estimate each.

use crate::budget::{Budget, BudgetFailure, Held, table_bytes, table_grown};
mod key;
mod regex;
use oxrdf::{BlankNode, Literal, NamedNode, Term};
pub(crate) use regex::BudgetRegex;
use rustc_hash::FxHashMap;
use sparkles_core::id::{self, Id, Tag};
use sparkles_core::sparql::value::Value;
use sparkles_core::store::Snapshot;
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

#[cfg(test)]
thread_local! {
    // One-shot interleaving at the unlocked, fully prepared publication boundary.
    static REGEX_PREPARED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

/// Regular expressions compiled for `regex` with a pattern that is not a constant, kept
/// at most: a full cache starts over.
const REGEX_CACHE: usize = 256;
/// The heap of a key's map entry besides the key's bytes, and of a table slot.
const ENTRY_BYTES: usize = size_of::<(Vec<u8>, u64)>();
const SLOT_BYTES: usize = size_of::<OnceLock<LocalTerm>>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Iri,
    BNode,
    Literal,
    /// RDF 1.2 triple term: valid as an object only
    Triple,
    Other,
}

/// Kind of a vocabulary key (`<iri`, `"literal`, `(triple`).
fn key_kind(k: &[u8]) -> Kind {
    match k.first() {
        Some(b'<') => Kind::Iri,
        Some(b'"') => Kind::Literal,
        Some(b'(') => Kind::Triple,
        _ => Kind::Other,
    }
}

#[derive(Clone, Debug)]
pub(crate) enum LocalTerm {
    /// vocabulary key of an IRI or literal
    Key(Vec<u8>),
    /// a blank node that must be created when writing
    BNode,
    /// A rule-level constant, keyed by its original label rather than a session slot.
    RuleBNode(Vec<u8>),
}

#[derive(Default)]
struct Locals {
    map: FxHashMap<Vec<u8>, u64>,
    /// local ids handed out
    len: u64,
}

/// The local terms by local id, append-only, read without a lock: segment `k` holds
/// `SEGMENT << k` slots.
struct LocalTable {
    segments: Vec<OnceLock<Box<[OnceLock<LocalTerm>]>>>,
}

const SEGMENT: u64 = 1024;

impl LocalTable {
    fn new() -> LocalTable {
        LocalTable {
            segments: (0..40).map(|_| OnceLock::new()).collect(),
        }
    }

    fn at(i: u64) -> (usize, usize) {
        let k = 63 - (i / SEGMENT + 1).leading_zeros() as u64;
        (k as usize, (i - SEGMENT * ((1 << k) - 1)) as usize)
    }

    fn get(&self, i: u64) -> Option<&LocalTerm> {
        let (k, off) = Self::at(i);
        self.segments.get(k)?.get()?.get(off)?.get()
    }

    /// The bytes slot `i` makes the table allocate: its segment's, when it is the
    /// segment's first.
    fn growth(&self, i: u64) -> usize {
        let (k, _) = Self::at(i);
        match self.segments.get(k) {
            Some(s) if s.get().is_none() => (SEGMENT << k) as usize * SLOT_BYTES,
            _ => 0,
        }
    }

    /// Set slot `i` (each slot once, by the holder of the write lock).
    fn set(&self, i: u64, t: LocalTerm) {
        let (k, off) = Self::at(i);
        let seg =
            self.segments[k].get_or_init(|| (0..(SEGMENT << k)).map(|_| OnceLock::new()).collect());
        let _ = seg[off].set(t);
    }
}

/// Term context shared by all evaluation threads.
pub(crate) struct Terms {
    pub snap: Arc<Snapshot>,
    analysis: Option<Arc<crate::analysis::AnalysisContext>>,
    analysis_round: AtomicUsize,
    /// base vocabulary id range `[lo, hi)` holding literals
    lit_range: (u64, u64),
    locals: RwLock<Locals>,
    table: LocalTable,
    regex_cache: Mutex<RegexCache>,
    pub rdf_first: u64,
    pub rdf_rest: u64,
    pub rdf_nil: u64,
    pub now: u64,
    // Payload fields above are destroyed before the final owned credit returns.
    credit: TermCredit,
}

/// Optional cache payload owns its keys/table separately from dictionary credit.
/// Compiled expressions have their own last-Arc owner.
#[derive(Default)]
pub(crate) struct RegexCache {
    values: FxHashMap<String, Option<Arc<BudgetRegex>>>,
    held: Option<Held>,
}

struct TermCredit {
    budget: RwLock<Arc<Budget>>,
    charged: AtomicUsize,
}
impl Drop for TermCredit {
    fn drop(&mut self) {
        self.budget
            .get_mut()
            .unwrap()
            .release(*self.charged.get_mut());
    }
}

impl Terms {
    pub fn new(snap: Arc<Snapshot>) -> Terms {
        Self::new_in(snap, &Budget::unlimited()).expect("unpooled initial terms")
    }

    /// Construct in the receiving budget before allocating the segment index or
    /// initial RDF/NOW keys. Temporary formatting overlaps its permanent copies.
    pub fn new_in(snap: Arc<Snapshot>, budget: &Arc<Budget>) -> Result<Terms, BudgetFailure> {
        let fixed = 40 * size_of::<OnceLock<Box<[OnceLock<LocalTerm>]>>>();
        let fixed_credit = Held::try_new_exact(budget, fixed)?;
        let lit_range = snap.generation.vocab.prefix_range(b"\"");
        let mut t = Terms {
            snap,
            analysis: None,
            analysis_round: AtomicUsize::new(0),
            lit_range,
            locals: RwLock::new(Locals::default()),
            table: LocalTable::new(),
            regex_cache: Mutex::new(RegexCache::default()),
            credit: TermCredit {
                budget: RwLock::new(budget.clone()),
                charged: AtomicUsize::new(fixed),
            },
            rdf_first: 0,
            rdf_rest: 0,
            rdf_nil: 0,
            now: 0,
        };
        let _ = fixed_credit.into_bytes();
        let mut rdf = [0; 3];
        for (i, suffix) in ["first", "rest", "nil"].iter().enumerate() {
            let bytes = crate::parser::RDF_NS
                .len()
                .checked_add(suffix.len())
                .and_then(|n| n.checked_mul(2))
                .and_then(|n| n.checked_add(size_of::<Term>()))
                .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
            let temporary = Held::try_new_exact(budget, bytes)?;
            let term = Term::NamedNode(NamedNode::new_unchecked(format!(
                "{}{suffix}",
                crate::parser::RDF_NS
            )));
            rdf[i] = t.try_id_for(&term)?;
            drop(term);
            drop(temporary);
        }
        [t.rdf_first, t.rdf_rest, t.rdf_nil] = rdf;
        // The existing scalar decode envelope uses256 bytes for an inline value.
        // It also covers the current DateTime's lexical formatting and typed value.
        let temporary = Held::try_new_exact(budget, 256)?;
        let now = Term::Literal(Literal::new_typed_literal(
            oxsdatatypes::DateTime::now().to_string(),
            oxrdf::vocab::xsd::DATE_TIME,
        ));
        t.now = t.try_id_for(&now)?;
        drop(now);
        drop(temporary);
        Ok(t)
    }

    pub fn set_analysis(
        &mut self,
        context: Arc<crate::analysis::AnalysisContext>,
    ) -> Result<(), BudgetFailure> {
        self.now = self.try_id_for(&context.now)?;
        self.analysis = Some(context);
        Ok(())
    }

    pub fn analysis_round(&self, round: usize) {
        if self.analysis.is_some() {
            self.analysis_round.store(round, Ordering::Relaxed);
        }
    }

    pub fn analysis_witness(
        &self,
        rule: &crate::engine::CRule,
        builtin: usize,
        output: usize,
        bindings: &[u64],
        skolem: Option<&[u64]>,
    ) -> Option<Result<u64, BudgetFailure>> {
        let context = self.analysis.as_ref()?;
        let mut key = Vec::new();
        if let Some(inputs) = skolem {
            // makeSkolem is shared by argument tuple throughout an ordinary program.
            key.push("skolem".into());
            key.extend(inputs.iter().map(|id| {
                self.term(*id)
                    .map_or_else(|| "unbound".into(), |term| term.to_string())
            }));
        } else {
            key.push(format!(
                "temp:{}:{output}:round:{}",
                rule.builtins[builtin].origin,
                self.analysis_round.load(Ordering::Relaxed)
            ));
            key.extend(bindings.iter().map(|id| {
                self.term(*id)
                    .map_or_else(|| "unbound".into(), |term| term.to_string())
            }));
        }
        Some(context.witness(key))
    }

    /// Charge the local terms to `budget` from now on, or fail when they do not fit.
    pub fn set_budget(&self, budget: &Arc<Budget>) -> Result<(), BudgetFailure> {
        loop {
            let (old, bytes) = {
                let old = self.credit.budget.read().unwrap();
                if Arc::ptr_eq(&old, budget) {
                    return Ok(());
                }
                (old.clone(), self.credit.charged.load(Ordering::Relaxed))
            };
            let prepared = Held::try_new_exact(budget, bytes)?;
            let mut current = self.credit.budget.write().unwrap();
            if !Arc::ptr_eq(&current, &old) || self.credit.charged.load(Ordering::Relaxed) != bytes
            {
                drop(current);
                drop(prepared);
                continue;
            }
            let retired_cache = std::mem::take(&mut *self.regex_cache.lock().unwrap());
            *current = budget.clone();
            drop(current);
            let _ = prepared.into_bytes();
            drop(retired_cache);
            old.release(bytes);
            return Ok(());
        }
    }

    /// Commit a whole-closure preparation. Its exclusive owner checked this
    /// source before changing any component; no charge or release occurs here.
    pub(crate) fn replace_credit(
        &self,
        source: &Arc<Budget>,
        held: Held,
    ) -> (Arc<Budget>, usize, RegexCache) {
        let bytes = self.heap_bytes();
        assert_eq!(held.bytes(), bytes);
        let mut current = self.credit.budget.write().unwrap();
        assert!(
            Arc::ptr_eq(&current, source),
            "unchanged prepared term owner"
        );
        let retired_cache = std::mem::take(&mut *self.regex_cache.lock().unwrap());
        let old = std::mem::replace(&mut *current, held.budget().clone());
        drop(current);
        let _ = held.into_bytes();
        (old, bytes, retired_cache)
    }

    /// The budget the local terms are charged to.
    pub fn budget(&self) -> Arc<Budget> {
        self.credit.budget.read().unwrap().clone()
    }

    /// The bytes the local terms are charged.
    pub fn heap_bytes(&self) -> usize {
        self.credit.charged.load(Ordering::Relaxed)
    }

    fn release(&self, n: usize) {
        let current = self.credit.budget.read().unwrap();
        let n = n.min(self.credit.charged.load(Ordering::Relaxed));
        self.credit.charged.fetch_sub(n, Ordering::Relaxed);
        let budget = current.clone();
        drop(current);
        budget.release(n);
    }

    /// Id of a term: inline, existing in the store, or a (new) local id, charged to the
    /// budget even past its limit (for the run to find at its next check).
    pub fn id_for(&self, t: &Term) -> Result<u64, BudgetFailure> {
        self.id_charged(t, false)
    }

    /// [`id_for`](Self::id_for), failing without a new term when it does not fit the
    /// budget.
    pub fn try_id_for(&self, t: &Term) -> Result<u64, BudgetFailure> {
        self.id_charged(t, true)
    }

    /// The id of the IRI or literal with vocabulary key `key`: in the store, else a
    /// (new) local id, failing without a new term when it does not fit the budget.
    pub fn try_id_for_key(&self, key: &[u8]) -> Result<u64, BudgetFailure> {
        if let Some(id) = self.snap.lookup_key(key) {
            return Ok(id.0);
        }
        let temporary = Held::try_new_exact(&self.budget(), key.len())?;
        let id = self.local_keyed(key.to_vec(), LocalTerm::Key(Vec::new()), true, None)?;
        drop(temporary);
        Ok(id)
    }

    fn id_charged(&self, t: &Term, strict: bool) -> Result<u64, BudgetFailure> {
        if let Some(id) = id::inline_id(t) {
            return Ok(id.0);
        }
        if let Term::BlankNode(b) = t {
            if let Some(id) = sparkles_core::store::parse_bnode_label(b.as_str()) {
                return Ok(id.0);
            }
            // Returned rule constants name their original lexical source. Build
            // that one lookup buffer directly, rather than a separate decoded Vec.
            if let Some(encoded) = b.as_str().strip_prefix("spkrule_")
                && encoded.len().is_multiple_of(2)
                && encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                let bytes = encoded
                    .len()
                    .checked_div(2)
                    .and_then(|n| n.checked_add(3))
                    .ok_or_else(|| {
                        BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow)
                    })?;
                let mut temporary = Held::new_exact_mode(&self.budget(), bytes, strict)?;
                let mut key = Vec::with_capacity(bytes);
                if key.capacity() > bytes {
                    if strict {
                        temporary.try_grow(key.capacity() - bytes)?;
                    } else {
                        temporary.grow(key.capacity() - bytes)?;
                    }
                }
                key.extend_from_slice(b"\0bn");
                for pair in encoded.as_bytes().as_chunks::<2>().0 {
                    let hex = |byte: u8| {
                        if byte.is_ascii_digit() {
                            byte - b'0'
                        } else {
                            byte.to_ascii_lowercase() - b'a' + 10
                        }
                    };
                    key.push(hex(pair[0]) * 16 + hex(pair[1]));
                }
                let existing = { self.locals.read().unwrap().map.get(&key).copied() };
                if let Some(id) = existing {
                    return Ok(id);
                }
            }
            return self.rule_bnode(b, strict);
        }
        let key = key::WorkingKey::new(t, &self.budget(), strict)?;
        if let Some(id) = self.snap.lookup_key(key.as_slice()) {
            return Ok(id.0);
        }
        key.intern(self, strict)
    }

    /// Compile a source constant without interpreting a response's encoded label.
    pub fn id_for_rule(&self, term: &Term) -> Result<u64, BudgetFailure> {
        match term {
            Term::BlankNode(node) => self.rule_bnode(node, false),
            _ => self.id_for(term),
        }
    }

    fn rule_bnode(&self, node: &BlankNode, strict: bool) -> Result<u64, BudgetFailure> {
        if let Some(id) = sparkles_core::store::parse_bnode_label(node.as_str()) {
            return Ok(id.0);
        }
        if let Some(context) = &self.analysis {
            let bytes = (2 * size_of::<String>())
                .checked_add("rule-blank".len())
                .and_then(|n| n.checked_add(node.as_str().len()))
                .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
            let temporary = Held::new_exact_mode(&self.budget(), bytes, strict)?;
            let result = context.witness(vec!["rule-blank".into(), node.as_str().into()]);
            drop(temporary);
            return result;
        }
        let bytes = node
            .as_str()
            .len()
            .checked_add(3)
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        let _temporary = Held::new_exact_mode(&self.budget(), bytes, strict)?;
        let mut key = Vec::with_capacity(bytes);
        key.extend_from_slice(b"\0bn");
        key.extend_from_slice(node.as_str().as_bytes());
        self.local_keyed(key, LocalTerm::RuleBNode(Vec::new()), strict, None)
    }

    /// The local id of `key`, made with `term` (a key term gets a copy of `key`) when
    /// it is new: its bytes, its entry, the map's growth and a new segment of the table
    /// are charged first.
    /// Intern `key`. A `carried` credit that already pays for the key buffer on
    /// the same budget moves into the permanent credit, so the moved buffer is
    /// not charged twice while it is interned.
    fn local_keyed(
        &self,
        key: Vec<u8>,
        term: LocalTerm,
        strict: bool,
        mut carried: Option<Held>,
    ) -> Result<u64, BudgetFailure> {
        loop {
            let (n, cap, len, grow, bytes) = {
                let l = self.locals.read().unwrap();
                if let Some(&id) = l.map.get(&key) {
                    return Ok(id);
                }
                let (cap, len) = (l.map.capacity(), l.map.len());
                let grow = if len == cap {
                    table_bytes(table_grown(cap, len, 1), ENTRY_BYTES)
                } else {
                    0
                };
                let copy = matches!(term, LocalTerm::Key(_) | LocalTerm::RuleBNode(_));
                let bytes = key
                    .len()
                    .checked_mul(1 + usize::from(copy))
                    .and_then(|n| n.checked_add(self.table.growth(l.len)))
                    .and_then(|n| n.checked_add(grow))
                    .ok_or_else(|| {
                        BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow)
                    })?;
                (l.len, cap, len, grow, bytes)
            };
            let budget = self.budget();
            let reuse = carried
                .as_ref()
                .filter(|held| Arc::ptr_eq(held.budget(), &budget))
                .map_or(0, |held| held.bytes().min(key.len()));
            let mut prepared = Held::new_exact_mode(&budget, bytes - reuse, strict)?;
            let current = self.credit.budget.read().unwrap();
            let mut l = self.locals.write().unwrap();
            if !Arc::ptr_eq(&current, &budget)
                || l.len != n
                || l.map.len() != len
                || l.map.capacity() != cap
            {
                drop(l);
                drop(current);
                drop(prepared);
                continue;
            }
            let value = match &term {
                LocalTerm::Key(_) => LocalTerm::Key(key.clone()),
                LocalTerm::RuleBNode(_) => LocalTerm::RuleBNode(key.clone()),
                LocalTerm::BNode => LocalTerm::BNode,
            };
            self.table.set(n, value);
            l.len += 1;
            let id = Id::local(n).0;
            l.map.insert(key, id);
            if reuse > 0
                && let Some(held) = carried.take()
            {
                prepared.absorb(held);
            }
            self.credit
                .charged
                .fetch_add(prepared.bytes(), Ordering::Relaxed);
            drop(l);
            drop(current);
            let _ = prepared.into_bytes();
            if grow > 0 {
                self.release(table_bytes(cap, ENTRY_BYTES));
            }
            return Ok(id);
        }
    }

    /// A fresh blank node (`makeTemp`), failing when it does not fit the budget.
    pub fn try_fresh_bnode(&self) -> Result<u64, BudgetFailure> {
        loop {
            let n = self.locals.read().unwrap().len;
            let bytes = self.table.growth(n);
            let budget = self.budget();
            let prepared = Held::try_new_exact(&budget, bytes)?;
            let current = self.credit.budget.read().unwrap();
            let mut l = self.locals.write().unwrap();
            if !Arc::ptr_eq(&current, &budget) || l.len != n {
                drop(l);
                drop(current);
                drop(prepared);
                continue;
            }
            self.table.set(n, LocalTerm::BNode);
            l.len += 1;
            self.credit.charged.fetch_add(bytes, Ordering::Relaxed);
            drop(l);
            drop(current);
            let _ = prepared.into_bytes();
            return Ok(Id::local(n).0);
        }
    }

    /// A blank node determined by `args` (`makeSkolem`), failing when it does not fit
    /// the budget.
    pub fn try_skolem(&self, args: &[u64]) -> Result<u64, BudgetFailure> {
        let bytes = args
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(3))
            .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
        let _temporary = Held::try_new_exact(&self.budget(), bytes)?;
        let mut key = Vec::with_capacity(bytes);
        key.extend_from_slice(b"\0sk");
        for a in args {
            key.extend_from_slice(&a.to_le_bytes());
        }
        self.local_keyed(key, LocalTerm::BNode, true, None)
    }

    /// A program-local blank constant cannot be recreated by inserting an RDF blank.
    pub fn is_rule_bnode(&self, id: u64) -> bool {
        Id(id).tag() == Tag::Local
            && matches!(
                self.table.get(Id(id).payload()),
                Some(LocalTerm::RuleBNode(_))
            )
    }

    pub fn local(&self, id: u64) -> Option<LocalTerm> {
        let i = Id(id);
        (i.tag() == Tag::Local)
            .then(|| self.table.get(i.payload()).cloned())
            .flatten()
    }

    /// The vocabulary key of a local id's IRI or literal, without cloning it, or
    /// `None` for a blank node or an id that is not local.
    pub(crate) fn local_key(&self, id: u64) -> Option<&[u8]> {
        let i = Id(id);
        if i.tag() != Tag::Local {
            return None;
        }
        match self.table.get(i.payload())? {
            LocalTerm::Key(k) => Some(k),
            _ => None,
        }
    }

    /// The kind of a local id's term, without cloning it.
    fn local_kind(&self, id: u64) -> Kind {
        match self.table.get(Id(id).payload()) {
            Some(LocalTerm::BNode | LocalTerm::RuleBNode(_)) => Kind::BNode,
            Some(LocalTerm::Key(k)) => key_kind(k),
            None => Kind::Other,
        }
    }

    pub fn kind(&self, id: u64) -> Kind {
        let i = Id(id);
        if i.is_inline() {
            // inline numbers, booleans, dates, … (whatever the library inlines)
            return Kind::Literal;
        }
        match i.tag() {
            Tag::BNode => Kind::BNode,
            Tag::Vocab => {
                let p = i.payload();
                let v = &self.snap.generation.vocab;
                if p >= self.lit_range.0 && p < self.lit_range.1 {
                    Kind::Literal
                } else if v.is_triple(p) {
                    Kind::Triple
                } else if v.is_iri(p) {
                    Kind::Iri
                } else {
                    Kind::Other
                }
            }
            Tag::Delta => self.snap.key(i).map_or(Kind::Other, |k| key_kind(&k)),
            Tag::Local => self.local_kind(id),
            _ => Kind::Other,
        }
    }

    pub fn term(&self, id: u64) -> Option<Term> {
        let i = Id(id);
        if i.tag() == Tag::Local {
            return Some(match self.local(id)? {
                LocalTerm::Key(k) => id::key_to_term(&k),
                LocalTerm::BNode => {
                    Term::BlankNode(BlankNode::new_unchecked(format!("r{:x}", i.payload())))
                }
                LocalTerm::RuleBNode(key) => {
                    use std::fmt::Write;
                    let mut label = String::with_capacity(8 + 2 * (key.len() - 3));
                    label.push_str("spkrule_");
                    for byte in &key[3..] {
                        write!(label, "{byte:02x}").expect("String write");
                    }
                    Term::BlankNode(BlankNode::new_unchecked(label))
                }
            });
        }
        self.snap.term(i)
    }

    /// Decode a summary term only after reserving reconstruction and RDF heap space.
    /// The returned holder covers the decoded term until the caller drops it.
    pub fn term_controlled(
        &self,
        raw: u64,
        budget: &Arc<Budget>,
    ) -> Result<(Option<Term>, Held), BudgetFailure> {
        let id = Id(raw);
        let mut decoded = Held::new(budget);
        let mut decode = |key: &[u8]| -> Result<Term, BudgetFailure> {
            decoded.try_grow(key.len().saturating_mul(8).saturating_add(256))?;
            Ok(id::key_to_term(key))
        };
        let term = match id.tag() {
            Tag::Local => match self.table.get(id.payload()) {
                Some(LocalTerm::Key(key)) => Some(decode(key)?),
                Some(LocalTerm::RuleBNode(key)) => {
                    decoded.try_grow(key.len().saturating_mul(8).saturating_add(256))?;
                    self.term(raw)
                }
                _ => {
                    decoded.try_grow(256)?;
                    self.term(raw)
                }
            },
            Tag::Vocab => {
                let mut reconstruction = Held::new(budget);
                self.snap
                    .generation
                    .vocab
                    .get_with_checked(
                        id.payload(),
                        |capacity| reconstruction.try_set(capacity.saturating_mul(2)),
                        decode,
                    )?
                    .transpose()?
            }
            Tag::Delta => self
                .snap
                .generation
                .dvocab
                .with(|vocab| vocab.get(id.payload()).map(&mut decode).transpose())?,
            _ => {
                decoded.try_grow(256)?;
                self.term(raw)
            }
        };
        Ok((term, decoded))
    }

    /// The vocabulary key of a term (`None` for a local blank node).
    pub fn key_of(&self, id: u64) -> Option<Vec<u8>> {
        let i = Id(id);
        match i.tag() {
            Tag::Local => match self.local(id)? {
                LocalTerm::Key(k) => Some(k),
                LocalTerm::BNode | LocalTerm::RuleBNode(_) => None,
            },
            Tag::Vocab | Tag::Delta => self.snap.key(i).map(|k| k.into_owned()),
            _ => self.snap.term(i).map(|t| id::term_key(&t)),
        }
    }

    /// Is this (possibly generalized) triple valid RDF?
    pub fn valid(&self, t: &[u64; 3]) -> bool {
        matches!(self.kind(t[0]), Kind::Iri | Kind::BNode)
            && self.kind(t[1]) == Kind::Iri
            && self.kind(t[2]) != Kind::Other
    }

    /// Read terms from a later snapshot of the same generation. Local terms that the
    /// store now has are returned with their store ids, for the caller to replace.
    pub fn rebase(&mut self, snap: Arc<Snapshot>) -> FxHashMap<u64, u64> {
        self.snap = snap;
        let mut moved = FxHashMap::default();
        let mut l = self.locals.write().unwrap();
        let Locals { map, .. } = &mut *l;
        let mut freed = 0;
        map.retain(|key, id| {
            if key.starts_with(b"\0") {
                return true;
            }
            match self.snap.lookup_key(key) {
                Some(s) => {
                    moved.insert(*id, s.0);
                    // the map's copy of the key; the table keeps its own
                    freed += key.len();
                    false
                }
                None => true,
            }
        });
        drop(l);
        self.release(freed);
        moved
    }

    pub fn value(&self, id: u64) -> Option<Value> {
        self.term(id).map(|t| Value::from_term(&t))
    }

    /// Lexical form used by `strConcat` / `uriConcat` / `regex` (Jena `lex()`).
    pub fn lexical(&self, id: u64) -> Option<String> {
        Some(match self.term(id)? {
            Term::NamedNode(n) => n.into_string(),
            Term::BlankNode(b) => b.into_string(),
            Term::Literal(l) => l.destruct().0,
            Term::Triple(t) => t.to_string(),
        })
    }

    /// Cache results share the expression's actual credit through the last Arc.
    pub fn try_regex(&self, pattern: &str) -> Result<Option<Arc<BudgetRegex>>, BudgetFailure> {
        type Entry = (String, Option<Arc<BudgetRegex>>);
        loop {
            let (len, cap) = {
                let mut cache = self.regex_cache.lock().unwrap();
                if let Some(regex) = cache.values.get(pattern) {
                    return Ok(regex.clone());
                }
                if cache.values.len() >= REGEX_CACHE {
                    let retired = std::mem::take(&mut *cache);
                    drop(cache);
                    drop(retired);
                    continue;
                }
                (cache.values.len(), cache.values.capacity())
            };
            let budget = self.budget();
            let next = table_grown(cap, len, 1);
            let table = if next != cap {
                table_bytes(next, size_of::<Entry>())
            } else {
                0
            };
            let bytes = pattern
                .len()
                .checked_add(table)
                .ok_or_else(|| BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow))?;
            let mut prepared = Held::try_new_exact(&budget, bytes)?;
            // Both the old and new table stay paid during replacement.
            let mut replacement = (next != cap)
                .then(|| FxHashMap::with_capacity_and_hasher(next, Default::default()));
            if let Some(map) = &replacement {
                let actual = table_bytes(map.capacity(), size_of::<Entry>());
                if actual > table {
                    prepared.try_grow(actual - table)?;
                } else {
                    prepared.shrink(table - actual);
                }
            }
            let regex = BudgetRegex::compile(pattern, &budget)?;
            let key = pattern.to_string();
            #[cfg(test)]
            {
                let hook = REGEX_PREPARED.with(|slot| slot.borrow_mut().take());
                if let Some(hook) = hook {
                    hook();
                }
            }
            let current = self.credit.budget.read().unwrap();
            let mut cache = self.regex_cache.lock().unwrap();
            if !Arc::ptr_eq(&current, &budget)
                || cache.values.len() != len
                || cache.values.capacity() != cap
            {
                drop(cache);
                drop(current);
                drop(key);
                drop(regex);
                drop(replacement);
                drop(prepared);
                continue;
            }
            // Another publication/eviction may restore the same len/capacity.
            // Preserve its canonical shared handle rather than overwrite it and
            // destroy its last regex owner while the cache is locked.
            if let Some(published) = cache.values.get(pattern) {
                let published = published.clone();
                drop(cache);
                drop(current);
                drop(key);
                drop(regex);
                drop(replacement);
                drop(prepared);
                return Ok(published);
            }
            let held = cache.held.get_or_insert_with(|| Held::new(&budget));
            // Arithmetic-only ownership transfer; no release callback under locks.
            if let Err(error) = held.try_merge(&mut prepared) {
                drop(cache);
                drop(current);
                return Err(error);
            }
            let retired_credit = replacement
                .as_ref()
                .map(|_| held.split(table_bytes(cap, size_of::<Entry>())));
            let retired_table = replacement.take().map(|mut replacement| {
                replacement.extend(cache.values.drain());
                std::mem::replace(&mut cache.values, replacement)
            });
            cache.values.insert(key, regex.clone());
            drop(cache);
            drop(current);
            drop(retired_table);
            drop(retired_credit);
            drop(prepared);
            return Ok(regex);
        }
    }
}
