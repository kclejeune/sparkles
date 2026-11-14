//! Immutable decoded query output. The final physical owner retains both its
//! payload and its original query credit. Borrowing does not transfer ownership;
//! an independent bounded copy must be admitted by the receiving OutputBudget.
use super::ctx::{Charge, Ctx, OutputCredit};
use super::depth;
use crate::error::{Error, Result};
use crate::id::Id;
use oxrdf::{NamedOrBlankNode, Term, TermRef};
use std::sync::Arc;

mod json;
mod native;
mod text;
pub use json::{OwnedBytes, OwnedJson};
pub use native::{NativeBudget, NativeReservation};

/// A receiving query's real accounting owner. This is not a numeric domain token.
/// Cloning it does not copy output or grant additional credit.
#[derive(Clone)]
pub struct OutputBudget {
    ctx: Arc<Ctx>,
}

impl OutputBudget {
    pub(crate) fn new(ctx: Arc<Ctx>) -> Self {
        Self { ctx }
    }

    pub fn check(&self) -> Result<()> {
        self.ctx.check()
    }

    /// Reserve before a concrete receiver allocates or grows its physical storage.
    /// Keep its payload before this final non-Clone guard in the owning structure.
    pub fn reserve(&self, bytes: u64) -> Result<OutputReservation> {
        self.ctx.check()?;
        Ok(OutputReservation {
            credit: OutputCredit::receiving(&self.ctx, bytes)?,
        })
    }

    /// A receiving account over a genuine source. This confers no authority and
    /// validates the actual Store; the original source Preparation remains the root.
    pub fn receiving_source(
        source: crate::store::BoundReadSource,
        store: &crate::store::Store,
        opts: &super::QueryOptions,
    ) -> Result<Self> {
        source.validate_query_instance(store)?;
        Self::receiving_prepared(source, opts, false)
    }
    fn receiving_prepared(
        source: crate::store::BoundReadSource,
        opts: &super::QueryOptions,
        just_prepared: bool,
    ) -> Result<Self> {
        source.query_input().2.check()?;
        let snapshot = source.query_input().0.clone();
        let policy = just_prepared
            .then(|| source.query_input().1.cloned())
            .flatten();
        let ctx = Arc::new(super::make_ctx_input_policy(
            snapshot,
            opts,
            None,
            None,
            Some(source),
            policy.as_ref(),
        )?);
        ctx.attach_owner();
        ctx.check()?;
        Ok(Self { ctx })
    }

    /// Borrow this actual receiver for native byte metadata and framing.
    pub fn native_budget(&self) -> Result<NativeBudget> {
        NativeBudget::from_output(self)
    }

    /// Copy text into this actual receiving account before allocating its backing.
    pub fn text(&self, text: &str) -> Result<OwnedText> {
        self.format_text(text)
    }

    /// The built-in RDF formatter is counted before allocation. Literal summaries
    /// retain their lexical value, matching the algorithm summary text contract.
    pub fn term_text(&self, term: &Term) -> Result<OwnedText> {
        if let Term::Literal(value) = term {
            return self.text(value.value());
        }
        depth::with_stack(term_depth(term), || self.format_text(term))
    }

    /// Make a genuinely receiving-owned copy of a borrowed RDF term. The source
    /// must remain live until this call completes; every payload byte is admitted first.
    pub fn copy_term(&self, term: TermRef<'_>) -> Result<OwnedTerm> {
        let mut cursor = term;
        let mut bytes = 0u64;
        let mut nesting = 0usize;
        loop {
            self.ctx.check()?;
            let payload = match cursor {
                TermRef::NamedNode(n) => n.as_str().len(),
                TermRef::BlankNode(b) => b.as_str().len(),
                TermRef::Literal(l) => l
                    .value()
                    .len()
                    .checked_add(l.datatype().as_str().len())
                    .and_then(|n| n.checked_add(l.language().map_or(0, str::len)))
                    .ok_or_else(|| self.overflow())?,
                TermRef::Triple(t) => {
                    let subject = match &t.subject {
                        NamedOrBlankNode::NamedNode(n) => n.as_str().len(),
                        NamedOrBlankNode::BlankNode(b) => b.as_str().len(),
                    };
                    let size = subject
                        .checked_add(t.predicate.as_str().len())
                        .and_then(|n| n.checked_add(std::mem::size_of::<oxrdf::Triple>()))
                        .and_then(|n| n.checked_add(128))
                        .ok_or_else(|| self.overflow())?;
                    bytes = bytes
                        .checked_add(u64::try_from(size).map_err(|_| self.overflow())?)
                        .ok_or_else(|| self.overflow())?;
                    nesting = nesting.checked_add(1).ok_or_else(|| self.overflow())?;
                    cursor = t.object.as_ref();
                    continue;
                }
            };
            bytes = bytes
                .checked_add(u64::try_from(payload).map_err(|_| self.overflow())?)
                .and_then(|n| n.checked_add(128))
                .ok_or_else(|| self.overflow())?;
            break;
        }
        let rows = self.build(1, 1, |_, _, held| {
            held.add(bytes)?;
            #[cfg(test)]
            {
                use crate::algo::memory::probe;
                probe::at(probe::Point::Allocation("borrowed-output-term", 1));
                if probe::fails("borrowed-output-term") {
                    return Err(Error::invalid(
                        "injected borrowed output allocation failure",
                    ));
                }
            }
            self.ctx.check()?;
            Ok(Some(depth::with_stack(nesting, || term.into_owned())))
        })?;
        Ok(OwnedTerm {
            inner: rows.inner,
            cell: 0,
        })
    }

    pub(super) fn decode(&self, id: Id) -> Result<Option<OwnedTerm>> {
        let rows = self.build(1, 1, |_, _, credit| self.ctx.output_term(id, credit))?;
        Ok(rows.inner.terms[0].as_ref().map(|_| OwnedTerm {
            inner: rows.inner.clone(),
            cell: 0,
        }))
    }

    pub(super) fn decode_rows(
        &self,
        rows: usize,
        width: usize,
        mut id: impl FnMut(usize, usize) -> Id,
    ) -> Result<OwnedRows> {
        self.build(rows, width, |r, c, credit| {
            self.ctx.output_term(id(r, c), credit)
        })
    }

    /// The template loader's separately owned graph scratch. Its sink borrows
    /// the complete Triple while this immutable owner retains actual admission.
    pub(crate) fn relabel_graph_triple(
        &self,
        triple: &oxrdf::Triple,
        part: usize,
        batch: u64,
        query: usize,
    ) -> Result<OwnedTerm> {
        use std::fmt::Write;
        self.ctx.check()?;
        let mut nodes = 1usize;
        let mut object = &triple.object;
        while let Term::Triple(inner) = object {
            self.ctx.check()?;
            nodes = nodes.checked_add(1).ok_or_else(|| self.overflow())?;
            object = &inner.object;
        }
        let rows = self.build(1, 1, |_, _, credit| {
            let subject = match &triple.subject {
                NamedOrBlankNode::NamedNode(node) => node.as_str().len(),
                NamedOrBlankNode::BlankNode(node) => node.as_str().len(),
            };
            // Each level has at most one subject blank node, plus one leaf
            // object. The decimal scalar prefix is at most64 bytes. Its own
            // fixed scratch and all possible prefix expansions are prepaid.
            let prefix_bytes = nodes
                .checked_add(2)
                .and_then(|n| n.checked_mul(64))
                .ok_or_else(|| self.overflow())?;
            let root = subject
                .checked_add(triple.predicate.as_str().len())
                .and_then(|n| n.checked_add(std::mem::size_of::<oxrdf::Triple>()))
                .and_then(|n| n.checked_add(prefix_bytes))
                .and_then(|n| n.checked_add(128))
                .and_then(|n| u64::try_from(n).ok())
                .ok_or_else(|| self.overflow())?;
            credit.add(
                root.checked_add(term_bytes(&triple.object).ok_or_else(|| self.overflow())?)
                    .ok_or_else(|| self.overflow())?,
            )?;
            #[cfg(test)]
            {
                use crate::algo::memory::probe;
                probe::at(probe::Point::Allocation("relabelled-graph-scratch", 1));
                if probe::fails("relabelled-graph-scratch") {
                    return Err(Error::invalid("injected graph scratch allocation failure"));
                }
            }
            self.ctx.check()?;
            let mut prefix = String::new();
            prefix
                .try_reserve_exact(64)
                .map_err(|_| Error::invalid("template prefix allocation failed"))?;
            if prefix.capacity() != 64 {
                return Err(Error::invalid("template prefix capacity exceeds admission"));
            }
            write!(&mut prefix, "t{part}b{batch}q{query}x")
                .expect("writing bounded decimal scalars into String");
            fn blank(ctx: &Ctx, node: &oxrdf::BlankNode, prefix: &str) -> Result<oxrdf::BlankNode> {
                ctx.check()?;
                let len = prefix
                    .len()
                    .checked_add(node.as_str().len())
                    .ok_or_else(|| ctx.memory_exceeded(u64::MAX))?;
                let mut name = String::new();
                name.try_reserve_exact(len)
                    .map_err(|_| Error::invalid("template blank node allocation failed"))?;
                if name.capacity() != len {
                    return Err(Error::invalid(
                        "template blank node capacity exceeds admission",
                    ));
                }
                name.push_str(prefix);
                for (index, ch) in node.as_str().chars().enumerate() {
                    if index.is_multiple_of(1024) {
                        ctx.check()?;
                    }
                    name.push(ch);
                }
                Ok(oxrdf::BlankNode::new_unchecked(name))
            }
            fn copied(ctx: &Ctx, triple: &oxrdf::Triple, prefix: &str) -> Result<oxrdf::Triple> {
                ctx.check()?;
                let subject = match &triple.subject {
                    NamedOrBlankNode::NamedNode(node) => NamedOrBlankNode::NamedNode(node.clone()),
                    NamedOrBlankNode::BlankNode(node) => {
                        NamedOrBlankNode::BlankNode(blank(ctx, node, prefix)?)
                    }
                };
                let predicate = triple.predicate.clone();
                let object = match &triple.object {
                    Term::BlankNode(node) => Term::BlankNode(blank(ctx, node, prefix)?),
                    Term::Triple(inner) => Term::Triple(Box::new(copied(ctx, inner, prefix)?)),
                    value => value.clone(),
                };
                Ok(oxrdf::Triple::new(subject, predicate, object))
            }
            depth::with_stack(nodes, || {
                Ok(Some(Term::Triple(Box::new(copied(
                    &self.ctx, triple, &prefix,
                )?))))
            })
        })?;
        Ok(OwnedTerm {
            inner: rows.inner,
            cell: 0,
        })
    }

    fn build(
        &self,
        rows: usize,
        width: usize,
        mut decode: impl FnMut(usize, usize, &Charge<'_>) -> Result<Option<Term>>,
    ) -> Result<OwnedRows> {
        self.ctx.check()?;
        let count = rows.checked_mul(width).ok_or_else(|| self.overflow())?;
        let cells = count
            .checked_mul(std::mem::size_of::<Option<Term>>())
            .filter(|bytes| *bytes <= isize::MAX as usize)
            .ok_or_else(|| self.overflow())?;
        let bytes = cells
            .checked_add(std::mem::size_of::<RowsBacking>())
            .and_then(|n| n.checked_add(2 * std::mem::size_of::<usize>()))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| self.overflow())?;
        let credit = self.ctx.charge(bytes)?;
        // Partial payload storage drops before its actual admission on every error.
        let mut partial = PartialRows {
            terms: Vec::new(),
            nesting: 0,
        };
        #[cfg(test)]
        {
            use crate::algo::memory::probe;
            probe::at(probe::Point::Allocation("owned-output-container", count));
            if probe::fails("owned-output-container") {
                return Err(Error::invalid("injected owned output allocation failure"));
            }
        }
        self.ctx.check()?;
        partial
            .terms
            .try_reserve_exact(count)
            .map_err(|_| Error::invalid("owned output allocation failed"))?;
        if partial.terms.capacity() != count {
            return Err(Error::invalid(
                "owned output capacity exceeds prepaid admission",
            ));
        }
        // Zero-width rows call no decoder and need no per-row work. Other copies
        // poll controls after at most1024 cells, including all-UNDEF inputs.
        if width != 0 {
            let mut unchecked = 1024usize;
            for r in 0..rows {
                for c in 0..width {
                    if unchecked == 0 {
                        self.ctx.check()?;
                        unchecked = 1024;
                    }
                    unchecked -= 1;
                    let term = decode(r, c, &credit)?;
                    if let Some(term) = &term {
                        partial.nesting = partial.nesting.max(term_depth(term));
                    }
                    partial.terms.push(term);
                }
            }
        }
        self.ctx.check()?;
        Ok(OwnedRows {
            inner: Arc::new(RowsBacking {
                terms: std::mem::take(&mut partial.terms),
                rows,
                width,
                nesting: partial.nesting,
                _credit: credit.into_output(&self.ctx),
            }),
        })
    }

    fn copy<'a>(
        &self,
        rows: usize,
        width: usize,
        mut cell: impl FnMut(usize, usize) -> Option<&'a Term>,
    ) -> Result<OwnedRows> {
        self.build(rows, width, |r, c, credit| {
            let Some(term) = cell(r, c) else {
                return Ok(None);
            };
            credit.add(term_bytes(term).ok_or_else(|| self.overflow())?)?;
            Ok(Some(depth::with_stack(term_depth(term), || term.clone())))
        })
    }

    fn overflow(&self) -> Error {
        self.ctx.memory_exceeded(u64::MAX)
    }
}

// This guard also protects nested partial terms when a later cell refuses.
struct PartialRows {
    terms: Vec<Option<Term>>,
    nesting: usize,
}
impl Drop for PartialRows {
    fn drop(&mut self) {
        depth::with_stack(self.nesting, || self.terms.clear());
    }
}

// Payload first, account credit last. No field is exposed by ownership.
struct RowsBacking {
    terms: Vec<Option<Term>>,
    rows: usize,
    width: usize,
    nesting: usize,
    _credit: OutputCredit,
}

impl Drop for RowsBacking {
    fn drop(&mut self) {
        depth::with_stack(self.nesting, || self.terms.clear());
    }
}

/// Mutable admission for a concrete physical receiver. Growth must precede
/// allocation; release_to is called only after the corresponding storage is freed.
pub struct OutputReservation {
    credit: OutputCredit,
}
impl OutputReservation {
    pub fn bytes(&self) -> u64 {
        self.credit.bytes()
    }
    pub fn grow_to(&mut self, bytes: u64) -> Result<()> {
        self.credit.check()?;
        if bytes > self.bytes() {
            self.credit.resize(bytes)?;
        }
        Ok(())
    }
    pub fn release_to(&mut self, bytes: u64) -> Result<()> {
        if bytes > self.bytes() {
            return Err(Error::invalid("receiving release cannot grow admission"));
        }
        self.credit.resize(bytes)
    }
}

/// One application-facing Snapshot operation with a real shared preparation.
/// It creates no evidence-history or current-authority proof. Checked sources use
/// receiving_source/query_source and continue requiring the actual Store.
pub struct SnapshotOutputRoot {
    source: crate::store::BoundReadSource,
    budget: OutputBudget,
}
impl SnapshotOutputRoot {
    pub fn new(snapshot: Arc<crate::store::Snapshot>, opts: &super::QueryOptions) -> Result<Self> {
        Self::new_with_controls(snapshot, opts, None, None, None)
    }
    /// Intersect an enclosing operation's original controls without allocating a
    /// flattened cancellation list or renewing either absolute deadline.
    pub fn new_with_controls(
        snapshot: Arc<crate::store::Snapshot>,
        opts: &super::QueryOptions,
        deadline: Option<std::time::Instant>,
        cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
        cancellation: Option<crate::task::Cancellation>,
    ) -> Result<Self> {
        let preparation =
            crate::access::Preparation::for_query_controls(opts, deadline, cancel, cancellation)?;
        let unrestricted = crate::access::GraphAccess::all();
        let access = opts.graphs.as_deref().unwrap_or(&unrestricted);
        let prepared = access.masked_controlled(&snapshot, &preparation)?;
        let source = prepared.read_source();
        let budget = OutputBudget::receiving_prepared(source.clone(), opts, true)?;
        Ok(Self { source, budget })
    }
    pub fn budget(&self) -> &OutputBudget {
        &self.budget
    }
    /// Decode an ID from this root's actual physical input under its original
    /// accounting and controls. The returned cell retains that source and credit.
    pub fn term(&self, id: Id) -> Result<Option<OwnedTerm>> {
        self.budget.check()?;
        self.budget.decode(id)
    }
    /// Execute an already-owned parsed query under the same real root. Input AST
    /// and further DTO copies require their own caller/receiving admission.
    pub fn execute(
        &self,
        query: &spargebra::Query,
        opts: &super::QueryOptions,
    ) -> Result<super::QueryResult> {
        self.budget.check()?;
        super::execute_retained_source(self.source.clone(), query, opts, 0.0)
    }
}

/// A term view retaining the complete immutable allocation which owns its bytes.
/// Keeping one cell can retain the rest of its row collection; use try_copy_in
/// when that tradeoff is inappropriate and a destination budget can admit it.
#[derive(Clone)]
pub struct OwnedTerm {
    inner: Arc<RowsBacking>,
    cell: usize,
}

impl OwnedTerm {
    pub fn as_term(&self) -> &Term {
        self.inner.terms[self.cell].as_ref().expect("bound cell")
    }

    pub fn try_copy_in(&self, budget: &OutputBudget) -> Result<Self> {
        let rows = budget.copy(1, 1, |_, _| Some(self.as_term()))?;
        Ok(Self {
            inner: rows.inner,
            cell: 0,
        })
    }
}

impl PartialEq for OwnedTerm {
    fn eq(&self, other: &Self) -> bool {
        depth::with_stack(self.inner.nesting.max(other.inner.nesting), || {
            self.as_term() == other.as_term()
        })
    }
}
impl Eq for OwnedTerm {}
impl std::hash::Hash for OwnedTerm {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        depth::with_stack(self.inner.nesting, || {
            std::hash::Hash::hash(self.as_term(), state)
        });
    }
}
impl std::borrow::Borrow<Term> for OwnedTerm {
    fn borrow(&self) -> &Term {
        self.as_term()
    }
}
impl std::fmt::Display for OwnedTerm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        depth::with_stack(self.inner.nesting, || {
            std::fmt::Display::fmt(self.as_term(), f)
        })
    }
}

impl std::fmt::Debug for OwnedTerm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        depth::with_stack(self.inner.nesting, || self.as_term().fmt(f))
    }
}

/// A shallow row handle. Accessors borrow values; they cannot detach its Vec.
#[derive(Clone)]
pub struct OwnedRow {
    inner: Arc<RowsBacking>,
    row: usize,
}

impl OwnedRow {
    pub fn values(&self) -> &[Option<Term>] {
        let start = self.row * self.inner.width;
        &self.inner.terms[start..start + self.inner.width]
    }

    pub fn get(&self, column: usize) -> Result<Option<TermRef<'_>>> {
        self.as_row().get(column)
    }

    pub fn term(&self, column: usize) -> Result<Option<OwnedTerm>> {
        Ok(self.get(column)?.map(|_| OwnedTerm {
            inner: self.inner.clone(),
            cell: self.row * self.inner.width + column,
        }))
    }

    pub fn as_row(&self) -> RowRef<'_> {
        RowRef {
            terms: self.values(),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.width
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn try_copy_in(&self, budget: &OutputBudget) -> Result<Self> {
        let rows = budget.copy(1, self.inner.width, |_, c| self.values()[c].as_ref())?;
        Ok(Self {
            inner: rows.inner,
            row: 0,
        })
    }
}

struct TextBacking {
    text: String,
    _credit: NativeReservation,
}
/// Immutable text with its actual original receiving credit. Clones share bytes.
#[derive(Clone)]
pub struct OwnedText {
    inner: Arc<TextBacking>,
}
impl OwnedText {
    pub fn as_str(&self) -> &str {
        &self.inner.text
    }
    pub fn len(&self) -> usize {
        self.as_str().len()
    }
    pub fn is_empty(&self) -> bool {
        self.as_str().is_empty()
    }
    pub fn try_copy_in(&self, budget: &OutputBudget) -> Result<Self> {
        budget.text(self.as_str())
    }
    pub fn try_copy_in_native(&self, budget: &NativeBudget) -> Result<Self> {
        budget.text(self.as_str())
    }
}
impl std::fmt::Display for OwnedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl std::fmt::Debug for OwnedText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(f)
    }
}
impl AsRef<str> for OwnedText {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl serde::Serialize for OwnedText {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// A non-owning row view. Its terms cannot outlive the physical row owner.
#[derive(Clone, Copy)]
pub struct RowRef<'a> {
    terms: &'a [Option<Term>],
}

impl<'a> RowRef<'a> {
    pub fn values(self) -> &'a [Option<Term>] {
        self.terms
    }

    pub fn len(self) -> usize {
        self.terms.len()
    }
    pub fn is_empty(self) -> bool {
        self.terms.is_empty()
    }

    pub fn get(self, column: usize) -> Result<Option<TermRef<'a>>> {
        self.terms
            .get(column)
            .map(|term| term.as_ref().map(Term::as_ref))
            .ok_or_else(|| Error::invalid("query output column is out of bounds"))
    }

    pub fn iter(self) -> impl ExactSizeIterator<Item = Option<TermRef<'a>>> + 'a {
        self.terms
            .iter()
            .map(|term| term.as_ref().map(Term::as_ref))
    }
}

impl AsRef<[Option<Term>]> for OwnedRow {
    fn as_ref(&self) -> &[Option<Term>] {
        self.values()
    }
}
impl std::fmt::Debug for OwnedRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.values().fmt(f)
    }
}

/// Immutable paid rows, including zero-width rows. Clones share physical storage.
#[derive(Clone)]
pub struct OwnedRows {
    inner: Arc<RowsBacking>,
}

/// A consuming iterator retaining the actual immutable rows backing. Each item
/// is a shallow row handle; neither iteration nor cloning a row copies payload.
pub struct OwnedRowsIntoIter {
    rows: OwnedRows,
    next: usize,
}
impl Iterator for OwnedRowsIntoIter {
    type Item = OwnedRow;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.rows.len() {
            return None;
        }
        let row = self.next;
        self.next += 1;
        Some(OwnedRow {
            inner: self.rows.inner.clone(),
            row,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.rows.len() - self.next;
        (left, Some(left))
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        let left = self.rows.len() - self.next;
        if n >= left {
            self.next = self.rows.len();
            return None;
        }
        self.next += n;
        self.next()
    }

    fn count(self) -> usize {
        self.rows.len() - self.next
    }
}
impl ExactSizeIterator for OwnedRowsIntoIter {}
impl std::iter::FusedIterator for OwnedRowsIntoIter {}
impl IntoIterator for OwnedRows {
    type Item = OwnedRow;
    type IntoIter = OwnedRowsIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        OwnedRowsIntoIter {
            rows: self,
            next: 0,
        }
    }
}

impl OwnedRows {
    pub fn len(&self) -> usize {
        self.inner.rows
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn width(&self) -> usize {
        self.inner.width
    }
    pub fn row(&self, row: usize) -> Result<RowRef<'_>> {
        if row >= self.len() {
            return Err(Error::invalid("query output row is out of bounds"));
        }
        let start = row * self.width();
        Ok(RowRef {
            terms: &self.inner.terms[start..start + self.width()],
        })
    }
    pub fn row_handle(&self, row: usize) -> Result<OwnedRow> {
        self.row(row)?;
        Ok(OwnedRow {
            inner: self.inner.clone(),
            row,
        })
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = OwnedRow> + '_ {
        (0..self.len()).map(|row| OwnedRow {
            inner: self.inner.clone(),
            row,
        })
    }
    pub fn try_copy_in(&self, budget: &OutputBudget) -> Result<Self> {
        budget.copy(self.len(), self.width(), |r, c| {
            self.inner.terms[r * self.width() + c].as_ref()
        })
    }
}

impl std::fmt::Debug for OwnedRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

pub(super) fn term_depth(mut term: &Term) -> usize {
    let mut nesting = 0;
    while let Term::Triple(triple) = term {
        nesting += 1;
        term = &triple.object;
    }
    nesting
}

// Conservative owned string/box payload envelope. The outer Term slots are paid
// by build; no allocation occurs while calculating this admission.
pub(super) fn term_bytes(mut term: &Term) -> Option<u64> {
    let mut bytes = 0u64;
    loop {
        let text = match term {
            Term::NamedNode(node) => node.as_str().len(),
            Term::BlankNode(node) => node.as_str().len(),
            Term::Literal(literal) => literal
                .value()
                .len()
                .checked_add(literal.datatype().as_str().len())?
                .checked_add(literal.language().map_or(0, str::len))?,
            Term::Triple(triple) => {
                let subject = match &triple.subject {
                    NamedOrBlankNode::NamedNode(node) => node.as_str().len(),
                    NamedOrBlankNode::BlankNode(node) => node.as_str().len(),
                };
                let node = subject
                    .checked_add(triple.predicate.as_str().len())?
                    .checked_add(std::mem::size_of::<oxrdf::Triple>())?;
                bytes = bytes.checked_add(u64::try_from(node).ok()?.checked_add(128)?)?;
                term = &triple.object;
                continue;
            }
        };
        return bytes.checked_add(u64::try_from(text).ok()?.checked_add(128)?);
    }
}

#[cfg(test)]
mod tests;
