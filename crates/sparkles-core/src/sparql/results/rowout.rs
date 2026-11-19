//! The output buffer of the result writers and the record of where their rows end.
//!
//! [`RowOut`] collects a writer's bytes in pieces of [`OUTPUT_BUFFER`] bytes on the way to
//! the caller's writer, and records where each row ends as an `(offset, rows)` mark. A
//! row counts in [`RowCount`] once the caller's writer has taken the bytes up to its end.
//! A response transport that buffers and sends the bytes on can also hold a
//! [`RowLedger`], in which a row counts only once the transport has handed over the
//! bytes up to its end, so a body that stops partway reports the rows it delivered.

use crate::error::{Error, Result};
use crate::sparql::ctx::{Charge, Ctx};
use std::cell::Cell;
use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The bytes [`RowOut`] collects before it writes them on. Serializers write a few bytes
/// at a time, and each write that went straight to the response writer passed its size
/// limit, cancellation check and channel buffer.
pub(in crate::sparql) const OUTPUT_BUFFER: usize = 8 << 10;

/// The marks a [`RowOut`] makes room for first, with its buffer. The vector grows by
/// doubling, and it holds at most one mark per distinct offset in the buffer, so at most
/// `OUTPUT_BUFFER + 1`.
pub(in crate::sparql) const FIRST_MARKS: usize = 64;

pub(in crate::sparql) const MARK_BYTES: u64 = std::mem::size_of::<(usize, u64)>() as u64;

/// The rows of a result writer's output that reached the caller's writer whole, and
/// optionally the transport's [`RowLedger`].
#[derive(Default)]
pub struct RowCount {
    taken: Cell<u64>,
    ledger: Option<RowLedger>,
}

impl RowCount {
    pub fn new() -> Self {
        Self::default()
    }

    /// A count whose writer also records its row ends in `ledger`.
    pub fn with_ledger(ledger: RowLedger) -> Self {
        Self {
            taken: Cell::new(0),
            ledger: Some(ledger),
        }
    }

    /// The rows whose bytes the caller's writer has taken, including a writer that took
    /// them in parts and then failed.
    pub fn get(&self) -> u64 {
        self.taken.get()
    }

    fn add(&self, n: u64) {
        self.taken.set(self.taken.get() + n);
    }
}

/// Where rows end in a response stream, shared by the result writer and the transport
/// that sends its bytes on. The writer registers each row end as a stream offset before
/// it writes the bytes it lies in. The transport reports the bytes it has copied into its
/// buffer and, at each send that enqueued successfully, the bytes it has handed over. A
/// row counts as delivered once the bytes up to its end have been handed over, whenever
/// its mark arrives.
#[derive(Clone, Default)]
pub struct RowLedger(Arc<LedgerShared>);

#[derive(Default)]
struct LedgerShared {
    /// the bytes the transport has copied into its buffer, written on every copy without
    /// a lock
    copied: AtomicU64,
    state: parking_lot::Mutex<LedgerState>,
}

#[derive(Default)]
struct LedgerState {
    /// the bytes handed over
    sent: u64,
    /// rows whose ends were copied but not handed over
    in_buffer: u64,
    /// rows whose ends were handed over
    delivered: u64,
    /// marks past the bytes copied when they were registered, in stream order. They are
    /// at most those of one piece of the writer and the late marks before it, since each
    /// registration and each send collapses the ones that the copies have reached.
    pending: VecDeque<(u64, u64)>,
    /// a writer registered with this ledger
    used: bool,
}

impl RowLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// The transport has copied `total` bytes of the stream into its buffer.
    #[inline]
    pub fn copied(&self, total: u64) {
        self.0.copied.store(total, Ordering::Relaxed);
    }

    /// The transport has handed over the stream's first `total` bytes, every byte it had
    /// copied, in a send that enqueued.
    pub fn handed_over(&self, total: u64) {
        let mut s = self.0.state.lock();
        s.sent = s.sent.max(total);
        let mut delivered = std::mem::take(&mut s.in_buffer);
        while let Some(&(offset, n)) = s.pending.front()
            && offset <= total
        {
            s.pending.pop_front();
            delivered += n;
        }
        s.delivered += delivered;
    }

    /// The rows whose bytes were handed over, or `None` when no writer recorded its rows
    /// here.
    pub fn delivered(&self) -> Option<u64> {
        let s = self.0.state.lock();
        s.used.then_some(s.delivered)
    }

    /// The stream offset at which a writer starts, which marks it as recording here.
    fn start(&self) -> u64 {
        self.0.state.lock().used = true;
        self.0.copied.load(Ordering::Relaxed)
    }

    /// Register row ends, `(stream offset, rows)` in stream order. Each counts by where
    /// its offset lies: handed over, copied, or not yet copied.
    fn register(&self, marks: impl Iterator<Item = (u64, u64)>) {
        let copied = self.0.copied.load(Ordering::Relaxed);
        let mut s = self.0.state.lock();
        while let Some(&(offset, n)) = s.pending.front()
            && offset <= copied
        {
            s.pending.pop_front();
            s.in_buffer += n;
        }
        for (offset, n) in marks {
            if offset <= s.sent {
                s.delivered += n;
            } else if offset <= copied {
                s.in_buffer += n;
            } else {
                s.pending.push_back((offset, n));
            }
        }
    }
}

/// The writer that the result serializers write to: their output in pieces of
/// [`OUTPUT_BUFFER`] bytes on the way to the caller's writer. Its type is the engine's,
/// so the serializers are compiled with the engine and not once per caller's writer in
/// the crate that calls them, where their speed moved with unrelated changes.
///
/// Row ends are recorded in one of two modes. A foreign serializer, which owns this
/// writer while it writes, counts its rows in the staged counter after writing each of
/// them, and every write looks at that counter (the staged mode). A writer of the
/// engine's own calls [`RowOut::begin_row`] before a row and [`RowOut::end_row`] after it,
/// which costs nothing per write (the explicit mode). Either way a mark `(offset, rows)`
/// says that `rows` rows end at `offset` in the buffer.
///
/// The buffer and the marks are charged to the query before they are allocated, when the
/// writer has a query context: the whole buffer and the first marks at the first byte, in
/// one charge, and each growth of the marks after. The charge borrows the context, so an
/// eager query charges its fast memory account and does not create the query ledger.
pub(crate) struct RowOut<'a> {
    inner: &'a mut dyn Write,
    buf: Vec<u8>,
    /// rows ending at an offset of `buf`, in order, at most one entry per offset
    marks: Vec<(usize, u64)>,
    staged: &'a Cell<u64>,
    /// `staged` when it was last looked at
    rows: u64,
    count: &'a RowCount,
    /// the stream offset at which this writer started, for the ledger
    base: u64,
    /// the bytes the caller's writer has taken
    taken: u64,
    /// the caller's writer failed, so nothing more goes to it
    failed: bool,
    /// an error that a [`Write`] call could report only as an I/O error, such as a
    /// refused memory charge, returned in its place
    refused: Option<Error>,
    ctx: Option<&'a Ctx>,
    /// the reservation for `buf` and `marks`, declared after them so that they drop first
    charge: Option<Charge<'a>>,
}

impl<'a> RowOut<'a> {
    fn new(
        inner: &'a mut dyn Write,
        staged: &'a Cell<u64>,
        count: &'a RowCount,
        ctx: Option<&'a Ctx>,
    ) -> Self {
        let base = count.ledger.as_ref().map_or(0, RowLedger::start);
        RowOut {
            inner,
            buf: Vec::new(),
            marks: Vec::new(),
            staged,
            rows: staged.get(),
            count,
            base,
            taken: 0,
            failed: false,
            refused: None,
            ctx,
            charge: None,
        }
    }

    /// Cover `bytes` of buffer and marks before they are allocated.
    fn cover(&mut self, bytes: u64) -> Result<()> {
        let Some(ctx) = self.ctx else {
            return Ok(());
        };
        match &self.charge {
            Some(charge) if charge.bytes() >= bytes => Ok(()),
            Some(charge) => charge.resize(bytes),
            None => {
                self.charge = Some(ctx.charge(bytes)?);
                Ok(())
            }
        }
    }

    fn held(&self, buf: usize, marks: usize) -> u64 {
        buf as u64 + marks as u64 * MARK_BYTES
    }

    /// Keep a refusal to return in place of the I/O error a [`Write`] call can give.
    #[cold]
    fn refuse(&mut self, e: Error) -> std::io::Error {
        let io = std::io::Error::other(e.to_string());
        self.refused.get_or_insert(e);
        io
    }

    /// Allocate the buffer and the first marks, under one charge, before the first byte.
    #[cold]
    #[inline(never)]
    fn start(&mut self) -> std::io::Result<()> {
        let marks = self.marks.capacity().max(FIRST_MARKS);
        if let Err(e) = self.cover(self.held(OUTPUT_BUFFER, marks)) {
            return Err(self.refuse(e));
        }
        self.buf.reserve_exact(OUTPUT_BUFFER);
        self.marks.reserve_exact(marks - self.marks.len());
        Ok(())
    }

    /// Make sure `marks` has a free slot, growing it under its charge.
    #[inline]
    fn reserve_mark(&mut self) -> std::io::Result<()> {
        if self.marks.len() < self.marks.capacity() {
            return Ok(());
        }
        self.grow_marks()
    }

    #[cold]
    #[inline(never)]
    fn grow_marks(&mut self) -> std::io::Result<()> {
        let marks = (self.marks.capacity() * 2).max(FIRST_MARKS);
        let buf = self.buf.capacity();
        if let Err(e) = self.cover(self.held(buf, marks)) {
            return Err(self.refuse(e));
        }
        self.marks.reserve_exact(marks - self.marks.len());
        Ok(())
    }

    /// `n` rows end where `buf` ends now. A mark at the same offset as the last merges
    /// with it, so this needs a free slot only for a new offset.
    #[inline]
    fn push_mark(&mut self, n: u64) {
        let offset = self.buf.len();
        match self.marks.last_mut() {
            Some(last) if last.0 == offset => last.1 += n,
            _ => {
                debug_assert!(
                    self.marks.len() < self.marks.capacity(),
                    "a row ends without a free mark slot"
                );
                self.marks.push((offset, n));
            }
        }
    }

    /// Explicit mode: make room for the next row's mark, before any of its bytes, so that
    /// a refused growth fails before the row starts and [`RowOut::end_row`] cannot fail.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn begin_row(&mut self) -> Result<()> {
        if self.marks.len() < self.marks.capacity() {
            return Ok(());
        }
        self.grow_marks()
            .map_err(|e| self.refused.take().unwrap_or(Error::Io(e)))
    }

    /// Explicit mode: `n` rows end here, `n` greater than one for a writer that counts
    /// several statements at once. It never allocates or fails.
    #[cfg_attr(not(test), allow(dead_code))]
    #[inline]
    pub(crate) fn end_row(&mut self, n: u64) {
        self.push_mark(n);
    }

    /// Staged mode: note the rows counted since the last look, which end where `buf`
    /// ends now.
    #[inline(always)]
    fn note(&mut self) -> std::io::Result<()> {
        let rows = self.staged.get();
        if rows != self.rows {
            return self.mark(rows);
        }
        Ok(())
    }

    /// Kept out of line so that the serializers' many small writes inline only the
    /// check. After the push it makes room for the next mark, so a refused growth fails
    /// the write that follows the row and never loses the mark of a row already written.
    #[inline(never)]
    fn mark(&mut self, rows: u64) -> std::io::Result<()> {
        let n = rows - self.rows;
        self.rows = rows;
        // a row with bytes has a slot already, since the first write makes room
        if self.marks.capacity() == 0 {
            self.grow_marks()?;
        }
        self.push_mark(n);
        self.reserve_mark()
    }

    /// [`Write::write`] of `b` when it does not fit in what is left of `buf`.
    #[inline(never)]
    fn write_full(&mut self, b: &[u8]) -> std::io::Result<usize> {
        // the buffer and the slot for the mark of the row these bytes belong to exist
        // before the bytes go anywhere
        if self.buf.capacity() == 0 {
            self.start()?;
            if b.len() < OUTPUT_BUFFER {
                self.buf.extend_from_slice(b);
                return Ok(b.len());
            }
        }
        self.drain()?;
        if b.len() >= OUTPUT_BUFFER {
            // no row ends inside one write, so this piece ends none
            let r = self.inner.write_all(b);
            self.failed |= r.is_err();
            r?;
            self.taken += b.len() as u64;
        } else {
            self.buf.extend_from_slice(b);
        }
        Ok(b.len())
    }

    /// Hand the marks to the ledger as stream offsets, before the piece they lie in is
    /// written.
    fn register(&mut self) {
        if let Some(ledger) = &self.count.ledger
            && !self.marks.is_empty()
        {
            let start = self.base + self.taken;
            ledger.register(
                self.marks
                    .iter()
                    .map(|&(offset, n)| (start + offset as u64, n)),
            );
        }
    }

    /// Write the collected bytes on. Each row counts as soon as the caller's writer has
    /// taken the bytes up to its end, also when it takes them in parts and fails before
    /// the last.
    fn drain(&mut self) -> std::io::Result<()> {
        if self.failed {
            return Err(std::io::Error::other("the output already failed"));
        }
        self.register();
        let mut done = 0;
        let mut mark = 0;
        loop {
            while let Some(&(end, n)) = self.marks.get(mark)
                && end <= done
            {
                self.count.add(n);
                mark += 1;
            }
            if done == self.buf.len() {
                break;
            }
            match self.inner.write(&self.buf[done..]) {
                Ok(0) => {
                    // the marks are registered and the rows taken are counted
                    self.failed = true;
                    self.marks.clear();
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                Ok(n) => {
                    done += n;
                    self.taken += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.failed = true;
                    self.marks.clear();
                    return Err(e);
                }
            }
        }
        self.buf.clear();
        self.marks.clear();
        Ok(())
    }
}

impl Write for RowOut<'_> {
    #[inline(always)]
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.note()?;
        if b.len() > self.buf.capacity() - self.buf.len() {
            return self.write_full(b);
        }
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }

    #[inline(always)]
    fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.write(b).map(|_| ())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.note()?;
        self.drain()?;
        self.inner.flush()
    }
}

/// Run `f`, a serializer, on a [`RowOut`] writer to `w`, and write what it left in the
/// buffer on. A serializer that fails still has the bytes before its failure written on,
/// as they were without the buffer, so `count` counts the rows that reached `w`. With a
/// query context `ctx`, the writer's buffer and marks are charged to that query.
pub(in crate::sparql) fn write_buffered(
    w: &mut dyn Write,
    count: &RowCount,
    ctx: Option<Arc<Ctx>>,
    f: &mut dyn FnMut(&mut RowOut<'_>, &Cell<u64>) -> Result<()>,
) -> Result<()> {
    let staged = Cell::new(0);
    let mut out = RowOut::new(w, &staged, count, ctx.as_deref());
    let result = f(&mut out, &staged);
    // A refused charge reached the serializer only as an I/O error, so that error is
    // replaced by the refusal. An error of the serializer's own, such as a decode
    // refusal or a cancellation, is the one reported, even when a charge made after it,
    // for the last row's mark or in the drain below, is refused.
    let result = match (result, out.refused.take()) {
        (Err(Error::Io(_)), Some(refused)) => Err(refused),
        (result, _) => result,
    };
    match result {
        Ok(()) => {
            // the rows counted after the last write end where the output ends, and a
            // refused mark for them still has the bytes before it written on
            let written = match out.note() {
                Ok(()) => out.drain(),
                Err(e) => {
                    if !out.failed {
                        let _ = out.drain();
                    }
                    Err(e)
                }
            };
            written.map_err(|e| out.refused.take().unwrap_or(Error::Io(e)))
        }
        Err(e) => {
            // the output stops here, with the bytes before the error written on and the
            // rows that ended in them counted
            let _ = out.note();
            if !out.failed {
                let _ = out.drain();
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests;
