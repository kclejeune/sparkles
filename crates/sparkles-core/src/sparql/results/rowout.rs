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

/// A slot of a [`RowLedger`]'s queue of marks not yet copied, which a writer with a
/// ledger charges for each of its own mark slots.
pub(in crate::sparql) const LEDGER_MARK_BYTES: u64 = std::mem::size_of::<(u64, u64)>() as u64;

/// [`RowOut`]'s open offset when no row or piece is open.
const CLOSED: usize = usize::MAX;

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
    /// at most those of one piece of the writer, since each registration first collapses
    /// the ones before the piece's start, so the writer reserves room for as many as it
    /// has mark slots, under its charge, and frees it when it drops.
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

    /// Register the row ends of a piece that starts at stream offset `start`,
    /// `(stream offset, rows)` in stream order. Each counts by where its offset lies:
    /// handed over, copied, or not yet copied. The marks of earlier pieces, whose bytes
    /// the transport has accepted, count as copied, so the queue holds at most this
    /// piece's marks.
    fn register(&self, start: u64, marks: impl Iterator<Item = (u64, u64)>) {
        let copied = self.0.copied.load(Ordering::Relaxed);
        let mut s = self.0.state.lock();
        let reached = copied.max(start);
        while let Some(&(offset, n)) = s.pending.front()
            && offset <= reached
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
                debug_assert!(
                    s.pending.len() < s.pending.capacity(),
                    "a mark past the reserved queue"
                );
                s.pending.push_back((offset, n));
            }
        }
    }

    /// Make room in the queue for `marks` marks, after the writer has charged it.
    fn reserve_pending(&self, marks: usize) {
        let mut s = self.0.state.lock();
        let more = marks.saturating_sub(s.pending.len());
        s.pending.reserve_exact(more);
    }

    /// The writer stops: marks the copies have reached count as copied, and the rest,
    /// whose bytes no transport accepted and nothing will write, are dropped with the
    /// queue's storage.
    fn settle(&self) {
        let copied = self.0.copied.load(Ordering::Relaxed);
        let mut s = self.0.state.lock();
        let pending = std::mem::take(&mut s.pending);
        s.in_buffer += pending
            .iter()
            .take_while(|&&(offset, _)| offset <= copied)
            .map(|&(_, n)| n)
            .sum::<u64>();
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
    /// explicit mode: the offset in `buf` at which the open row or header starts, or
    /// [`CLOSED`] between pieces and in the staged mode. One word, so that opening and
    /// closing a row each cost one store.
    open: usize,
    /// explicit mode: the open row outgrew the buffer, and `buf` is the scratch that
    /// collects it whole while the buffer waits in `spill`. It implies `moved`, so the end
    /// of a row tests `moved` alone.
    growing: bool,
    /// explicit mode: the open piece moved to the front of the buffer because it did not
    /// fit where it started, or started before the buffer existed. Such a piece of exactly
    /// the buffer's size goes on in one write at its end, as a serializer's write of that
    /// size would at once. Only the cold paths set it, and the end of the piece clears it.
    moved: bool,
    /// the scratch for a row longer than the buffer, or the buffer while one grows, charged
    /// with the buffer and kept for the next long row
    spill: Vec<u8>,
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
            open: CLOSED,
            growing: false,
            moved: false,
            spill: Vec::new(),
            ctx,
            charge: None,
        }
    }

    /// The query whose memory this writer is charged to, when there is one.
    pub(crate) fn ctx(&self) -> Option<&'a Ctx> {
        self.ctx
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

    /// The bytes of `buf` bytes of buffer and scratch and `marks` mark slots, with as many
    /// slots of the ledger's queue when the count has a ledger.
    fn held(&self, buf: usize, marks: usize) -> u64 {
        let slot = if self.count.ledger.is_some() {
            MARK_BYTES + LEDGER_MARK_BYTES
        } else {
            MARK_BYTES
        };
        buf as u64 + marks as u64 * slot
    }

    /// Give the ledger's queue as many slots as the marks have, under the same charge.
    fn reserve_pending(&self, marks: usize) {
        if let Some(ledger) = &self.count.ledger {
            ledger.reserve_pending(marks);
        }
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
        if let Err(e) = self.cover(self.held(OUTPUT_BUFFER + self.spill.capacity(), marks)) {
            return Err(self.refuse(e));
        }
        self.buf.reserve_exact(OUTPUT_BUFFER);
        self.marks.reserve_exact(marks - self.marks.len());
        self.reserve_pending(marks);
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
        let buf = self.buf.capacity() + self.spill.capacity();
        if let Err(e) = self.cover(self.held(buf, marks)) {
            return Err(self.refuse(e));
        }
        self.marks.reserve_exact(marks - self.marks.len());
        self.reserve_pending(marks);
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
    /// a refused growth fails before the row starts, and open the row.
    pub(crate) fn begin_row(&mut self) -> Result<()> {
        if self.marks.len() == self.marks.capacity() {
            self.grow_marks()
                .map_err(|e| self.refused.take().unwrap_or(Error::Io(e)))?;
        }
        self.begin_piece();
        Ok(())
    }

    /// Explicit mode: `n` rows end here, `n` greater than one for a writer that counts
    /// several statements at once. A row that outgrew the buffer, or that moved and fills
    /// it exactly, goes on here in one write, which can fail. Nothing else allocates or
    /// fails.
    #[inline]
    pub(crate) fn end_row(&mut self, n: u64) -> std::io::Result<()> {
        self.open = CLOSED;
        debug_assert!(self.moved || !self.growing);
        if self.moved {
            return self.end_whole(Some(n));
        }
        self.push_mark(n);
        Ok(())
    }

    /// Explicit mode: open a piece that is not a row, such as a header, which goes to
    /// the caller's writer whole like a row but ends none.
    #[inline]
    pub(crate) fn begin_piece(&mut self) {
        debug_assert!(
            !self.moved && !self.growing,
            "a piece begins inside another"
        );
        self.open = self.buf.len();
    }

    /// Explicit mode: close the piece [`RowOut::begin_piece`] opened.
    pub(crate) fn end_piece(&mut self) -> std::io::Result<()> {
        self.open = CLOSED;
        debug_assert!(self.moved || !self.growing);
        if self.moved {
            return self.end_whole(None);
        }
        Ok(())
    }

    /// The end of a piece that grew past the buffer or moved, with the mark of `rows` if
    /// it ends rows. A moved piece of exactly the buffer's size, the whole buffer now, goes
    /// on in one write as a serializer's write of that size does.
    #[cold]
    #[inline(never)]
    fn end_whole(&mut self, rows: Option<u64>) -> std::io::Result<()> {
        self.moved = false;
        if self.growing {
            return self.write_grown(rows);
        }
        if self.buf.len() == OUTPUT_BUFFER {
            debug_assert!(self.marks.is_empty());
            let r = self.inner.write_all(&self.buf);
            self.failed |= r.is_err();
            r?;
            self.taken += self.buf.len() as u64;
            self.buf.clear();
        }
        if let Some(n) = rows {
            self.push_mark(n);
        }
        Ok(())
    }

    /// Make room for `len` bytes in the scratch, which is `buf` while a row grows and
    /// `spill` otherwise, under the charge and before it grows. The growth is admitted
    /// with the old allocation too, which a moving reallocation holds until it has copied
    /// the bytes, and the charge falls back to the new capacity after it.
    #[cold]
    #[inline(never)]
    fn reserve_scratch(&mut self, len: usize) -> std::io::Result<()> {
        let (scratch, buffer) = if self.growing {
            (self.buf.capacity(), self.spill.capacity())
        } else {
            (self.spill.capacity(), self.buf.capacity())
        };
        if scratch >= len {
            return Ok(());
        }
        let cap = len.max(scratch * 2);
        let marks = self.marks.capacity();
        if let Err(e) = self.cover(self.held(buffer + scratch + cap, marks)) {
            return Err(self.refuse(e));
        }
        let scratch = if self.growing {
            &mut self.buf
        } else {
            &mut self.spill
        };
        scratch.reserve_exact(cap - scratch.len());
        let grown = scratch.capacity();
        let after = self.held(buffer + grown, marks);
        if let Some(charge) = &self.charge
            && let Err(e) = charge.resize(after)
        {
            return Err(self.refuse(e));
        }
        Ok(())
    }

    /// Explicit mode: forget the bytes of an open row or piece, which will not end.
    #[cold]
    fn drop_open(&mut self) {
        if self.growing {
            self.growing = false;
            std::mem::swap(&mut self.buf, &mut self.spill);
            self.spill.clear();
        } else if self.open != CLOSED {
            self.buf.truncate(self.open);
        }
        self.open = CLOSED;
        self.moved = false;
    }

    /// Write on the row or piece collected whole in the scratch, as one write, as a
    /// serializer's write of at least the buffer's size goes, and put the buffer back.
    /// Its mark, if it ends a row, lies at the start of the empty buffer, as after such a
    /// write in the staged mode.
    #[cold]
    #[inline(never)]
    fn write_grown(&mut self, rows: Option<u64>) -> std::io::Result<()> {
        self.growing = false;
        std::mem::swap(&mut self.buf, &mut self.spill);
        debug_assert!(self.buf.is_empty() && self.marks.is_empty());
        let r = self.inner.write_all(&self.spill);
        self.failed |= r.is_err();
        if r.is_ok() {
            self.taken += self.spill.len() as u64;
        }
        self.spill.clear();
        r?;
        if let Some(n) = rows {
            self.push_mark(n);
        }
        Ok(())
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
        if self.growing {
            self.reserve_scratch(self.buf.len() + b.len())?;
            self.buf.extend_from_slice(b);
            return Ok(b.len());
        }
        // the buffer and the slot for the mark of the row these bytes belong to exist
        // before the bytes go anywhere
        if self.buf.capacity() == 0 {
            self.start()?;
            // a piece open before the buffer existed counts as moved, so that one of
            // exactly the buffer's size goes on at its end
            self.moved = self.open != CLOSED;
            if b.len() < OUTPUT_BUFFER {
                self.buf.extend_from_slice(b);
                return Ok(b.len());
            }
        }
        if self.open != CLOSED {
            return self.write_open(self.open, b);
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

    /// Explicit mode: `b` does not fit, and a row or piece is open at `start`. The bytes
    /// before it go on and the open part moves to the front, so that every piece the
    /// caller's writer takes ends where a row ends, as with a serializer that writes each
    /// row at once. Then a row longer than the buffer is collected whole and goes on at
    /// its end in one write, so that a size limit refuses it whole.
    #[cold]
    #[inline(never)]
    fn write_open(&mut self, start: usize, b: &[u8]) -> std::io::Result<usize> {
        self.drain_to(start)?;
        self.open = 0;
        self.moved = true;
        if self.buf.len() + b.len() <= OUTPUT_BUFFER {
            self.buf.extend_from_slice(b);
            return Ok(b.len());
        }
        self.spill.clear();
        self.reserve_scratch(self.buf.len() + b.len())?;
        self.spill.extend_from_slice(&self.buf);
        self.spill.extend_from_slice(b);
        self.buf.clear();
        std::mem::swap(&mut self.buf, &mut self.spill);
        self.growing = true;
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
                start,
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
        debug_assert!(
            !self.growing,
            "a drain inside a row that outgrew the buffer"
        );
        self.drain_to(self.buf.len())
    }

    /// Write the first `end` collected bytes on and move the rest, part of a row with no
    /// mark yet, to the front.
    fn drain_to(&mut self, end: usize) -> std::io::Result<()> {
        if self.failed {
            return Err(std::io::Error::other("the output already failed"));
        }
        debug_assert!(self.marks.last().is_none_or(|&(offset, _)| offset <= end));
        self.register();
        let mut done = 0;
        let mut mark = 0;
        loop {
            while let Some(&(at, n)) = self.marks.get(mark)
                && at <= done
            {
                self.count.add(n);
                mark += 1;
            }
            if done == end {
                break;
            }
            match self.inner.write(&self.buf[done..end]) {
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
        self.buf.copy_within(end.., 0);
        self.buf.truncate(self.buf.len() - end);
        self.marks.clear();
        Ok(())
    }
}

impl Drop for RowOut<'_> {
    /// Free the ledger's queue, which runs before the charge that covers it drops.
    fn drop(&mut self) {
        if let Some(ledger) = &self.count.ledger {
            ledger.settle();
        }
    }
}

impl RowOut<'_> {
    /// Collect `b` without looking at the staged counter.
    #[inline(always)]
    fn put(&mut self, b: &[u8]) -> std::io::Result<()> {
        if b.len() > self.buf.capacity() - self.buf.len() {
            return self.write_full(b).map(|_| ());
        }
        self.buf.extend_from_slice(b);
        Ok(())
    }
}

impl Write for RowOut<'_> {
    #[inline(always)]
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.note()?;
        self.put(b)?;
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

/// A [`RowOut`] in the explicit mode, for a writer of the engine's own that marks each
/// row's start and end. Its writes skip the staged counter that a foreign serializer's
/// writes look at, so a byte costs only its copy into the buffer. Nothing may count rows in
/// the staged counter while a row is open through this handle.
pub(crate) struct Explicit<'o, 'a>(&'o mut RowOut<'a>);

impl<'o, 'a> Explicit<'o, 'a> {
    pub(crate) fn new(out: &'o mut RowOut<'a>) -> Self {
        Explicit(out)
    }

    /// The query whose memory an encoder's own state is charged to, when there is one.
    #[cfg(test)]
    pub(crate) fn ctx(&self) -> Option<&'a Ctx> {
        self.0.ctx()
    }

    /// See [`RowOut::begin_row`].
    #[inline]
    pub(crate) fn begin_row(&mut self) -> Result<()> {
        self.0.begin_row()
    }

    /// See [`RowOut::end_row`].
    #[inline]
    pub(crate) fn end_row(&mut self, n: u64) -> std::io::Result<()> {
        self.0.end_row(n)
    }

    /// See [`RowOut::begin_piece`].
    pub(crate) fn begin_piece(&mut self) {
        self.0.begin_piece()
    }

    /// See [`RowOut::end_piece`].
    pub(crate) fn end_piece(&mut self) -> std::io::Result<()> {
        self.0.end_piece()
    }

    #[inline(always)]
    pub(crate) fn put(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.0.put(b)
    }
}

impl Write for Explicit<'_, '_> {
    #[inline(always)]
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.put(b)?;
        Ok(b.len())
    }

    #[inline(always)]
    fn write_all(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.0.put(b)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        debug_assert!(self.0.open == CLOSED, "a flush inside an open row");
        self.0.drain()?;
        self.0.inner.flush()
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
            // rows that ended in them counted, but no part of a row that did not end
            out.drop_open();
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
