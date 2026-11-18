use super::*;
use crate::error::{Budget, BudgetKind};
use crate::sparql::{QueryOptions, QueryResult, query};
use crate::store::{Store, StoreOptions};

/// A response transport as the server's switch writer behaves: it copies what it is
/// given into its buffer, up to `room` bytes a call, and sends the buffer whole once it
/// holds `chunk` bytes. The send numbered `fail_send` fails and loses its chunk. The send
/// numbered `fail_after` enqueues and then fails, which the transport reports at its
/// next call.
struct Transport {
    ledger: RowLedger,
    buf: Vec<u8>,
    copied: u64,
    sent: u64,
    chunk: usize,
    room: usize,
    sends: usize,
    fail_send: Option<usize>,
    fail_after: Option<usize>,
    deferred: bool,
    failed: bool,
}

impl Transport {
    fn new(ledger: &RowLedger, chunk: usize, room: usize) -> Self {
        Transport {
            ledger: ledger.clone(),
            buf: Vec::new(),
            copied: 0,
            sent: 0,
            chunk,
            room,
            sends: 0,
            fail_send: None,
            fail_after: None,
            deferred: false,
            failed: false,
        }
    }

    fn send(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        self.sends += 1;
        self.buf.clear();
        if self.fail_send == Some(self.sends) {
            self.failed = true;
            return Err(std::io::Error::other("send failed"));
        }
        self.sent = self.copied;
        self.ledger.handed_over(self.sent);
        if self.fail_after == Some(self.sends) {
            self.deferred = true;
        }
        Ok(())
    }

    /// The final send of a body that ended.
    fn finish(&mut self) -> std::io::Result<()> {
        if self.deferred || self.failed {
            return Err(std::io::Error::other("failed earlier"));
        }
        self.send()
    }
}

impl Write for Transport {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.deferred || self.failed {
            return Err(std::io::Error::other("failed earlier"));
        }
        let n = b.len().min(self.room);
        self.buf.extend_from_slice(&b[..n]);
        self.copied += n as u64;
        self.ledger.copied(self.copied);
        if self.buf.len() >= self.chunk {
            self.send()?;
        }
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Rows of `row` bytes, each written in pieces of `piece` bytes and counted in the staged
/// mode, or recorded in the explicit mode.
fn rows(
    n: u64,
    row: usize,
    piece: usize,
    explicit: bool,
) -> impl FnMut(&mut RowOut<'_>, &Cell<u64>) -> Result<()> {
    move |w, staged| {
        for i in 0..n {
            if explicit {
                w.begin_row()?;
            }
            let line = format!("{i:0>width$}\n", width = row - 1);
            for p in line.as_bytes().chunks(piece) {
                w.write_all(p).map_err(Error::Io)?;
            }
            if explicit {
                w.end_row(1);
            } else {
                staged.set(staged.get() + 1);
            }
        }
        Ok(())
    }
}

/// The rows of `row` bytes whose ends lie within the first `bytes` bytes.
fn whole_rows(bytes: u64, row: usize) -> u64 {
    bytes / row as u64
}

#[test]
fn the_ledger_counts_the_rows_whose_bytes_were_handed_over() {
    for explicit in [false, true] {
        for (row, piece) in [(40, 7), (40, 40), (3, 1), (9000, 100), (20_000, 20_000)] {
            for (chunk, room) in [(64 << 10, usize::MAX), (1000, 333), (100_000, 4096)] {
                // rows numbered within their width
                let n = 400.min(10u64.pow(row.min(10) as u32 - 1));
                // a whole body
                let ledger = RowLedger::new();
                let count = RowCount::with_ledger(ledger.clone());
                let mut t = Transport::new(&ledger, chunk, room);
                write_buffered(&mut t, &count, None, &mut rows(n, row, piece, explicit)).unwrap();
                t.finish().unwrap();
                assert_eq!(count.get(), n);
                assert_eq!(ledger.delivered(), Some(n), "{explicit} {row} {chunk}");
                // a send that fails partway loses its chunk and the rows ending in it
                for fail in [1, 2, 5] {
                    let ledger = RowLedger::new();
                    let count = RowCount::with_ledger(ledger.clone());
                    let mut t = Transport::new(&ledger, chunk, room);
                    t.fail_send = Some(fail);
                    let r =
                        write_buffered(&mut t, &count, None, &mut rows(n, row, piece, explicit));
                    let _ = r.and_then(|()| t.finish().map_err(Error::Io));
                    assert_eq!(
                        ledger.delivered(),
                        Some(whole_rows(t.sent, row)),
                        "{explicit} {row} {piece} {chunk} {room} fail {fail}"
                    );
                    assert!(count.get() >= ledger.delivered().unwrap());
                }
                // a send that enqueued and then failed counts its rows
                for fail in [1, 3] {
                    let ledger = RowLedger::new();
                    let count = RowCount::with_ledger(ledger.clone());
                    let mut t = Transport::new(&ledger, chunk, room);
                    t.fail_after = Some(fail);
                    let r =
                        write_buffered(&mut t, &count, None, &mut rows(n, row, piece, explicit));
                    let _ = r.and_then(|()| t.finish().map_err(Error::Io));
                    assert_eq!(
                        ledger.delivered(),
                        Some(whole_rows(t.sent, row)),
                        "{explicit} {row} {piece} {chunk} {room} after {fail}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_ledger_without_a_writer_reports_nothing() {
    let ledger = RowLedger::new();
    ledger.copied(10);
    ledger.handed_over(10);
    assert_eq!(ledger.delivered(), None);
}

#[test]
fn late_marks_count_by_where_their_bytes_are() {
    let ledger = RowLedger::new();
    assert_eq!(ledger.start(), 0);
    // a mark not yet copied waits, one copied waits for the send, one sent counts at once
    ledger.register([(10, 1)].into_iter());
    ledger.copied(20);
    ledger.register([(15, 2), (25, 1)].into_iter());
    assert_eq!(ledger.delivered(), Some(0));
    ledger.handed_over(20);
    assert_eq!(ledger.delivered(), Some(3));
    ledger.register([(18, 4)].into_iter());
    assert_eq!(ledger.delivered(), Some(7));
    // a copy that reaches a pending mark moves it to the buffer, and a lost buffer never
    // counts it
    ledger.copied(30);
    ledger.register(std::iter::empty());
    assert_eq!(ledger.delivered(), Some(7));
    ledger.handed_over(30);
    assert_eq!(ledger.delivered(), Some(8));
}

#[test]
fn explicit_and_staged_marks_merge_at_one_offset() {
    let count = RowCount::new();
    let mut out = Vec::new();
    write_buffered(&mut out, &count, None, &mut |w, staged| {
        w.begin_row()?;
        w.write_all(b"subject").map_err(Error::Io)?;
        // two rows ending at one offset, and a grouped count
        w.end_row(1);
        w.begin_row()?;
        w.end_row(3);
        assert_eq!(w.marks, vec![(7, 4)]);
        staged.set(2);
        w.write_all(b"x").map_err(Error::Io)?;
        assert_eq!(w.marks, vec![(7, 6)]);
        Ok(())
    })
    .unwrap();
    assert_eq!(count.get(), 6);
    assert_eq!(out, b"subjectx");
}

#[test]
fn a_late_mark_after_a_direct_write_counts_when_the_next_write_fails() {
    // the row's bytes went straight through, and the next write is refused
    let count = RowCount::new();
    let big = vec![b'a'; OUTPUT_BUFFER + 1];
    let mut out = super::super::LimitedWriter::new(Vec::new(), Some(big.len() as u64 + 10), None);
    let r = write_buffered(&mut out, &count, None, &mut |w, staged| {
        w.write_all(&big).map_err(Error::Io)?;
        staged.set(1);
        w.write_all(&big).map_err(Error::Io)?;
        staged.set(2);
        Ok(())
    });
    assert!(r.is_err());
    assert_eq!(count.get(), 1);
}

fn small_answer() -> QueryResult {
    let store = Store::in_memory(StoreOptions::default());
    let options = QueryOptions {
        max_memory_bytes: Some(1 << 20),
        no_cache: true,
        ..Default::default()
    };
    query(
        store.snapshot(),
        "SELECT ?o WHERE { VALUES ?o { 1 } }",
        &options,
    )
    .unwrap()
}

/// The most the query can still charge.
fn room(ctx: &Arc<Ctx>) -> u64 {
    let (mut lo, mut hi) = (0u64, 1 << 21);
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if OwnedCharge::new(ctx, mid).is_ok() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Hold all of the query's memory but `room` bytes.
fn leave(r: &QueryResult, left: u64) -> OwnedCharge {
    let ctx = r.test_context();
    OwnedCharge::new(ctx, room(ctx) - left).unwrap()
}

fn memory_refusal(r: Result<()>) {
    match r {
        Err(Error::BudgetExceeded(Budget {
            kind: BudgetKind::Memory,
            ..
        })) => {}
        other => panic!("the refusal lost its type: {other:?}"),
    }
}

#[test]
fn the_buffer_is_charged_before_its_first_byte() {
    let r = small_answer();
    let _held = leave(&r, 100);
    let count = RowCount::new();
    let mut out = Vec::new();
    memory_refusal(write_buffered(
        &mut out,
        &count,
        Some(r.test_context().clone()),
        &mut rows(3, 10, 3, false),
    ));
    assert!(out.is_empty());
    assert_eq!(count.get(), 0);
}

#[test]
fn a_refused_growth_keeps_the_rows_written() {
    for explicit in [false, true] {
        for left in [600, 1200, 3000] {
            let r = small_answer();
            let _held = leave(&r, left);
            let count = RowCount::new();
            let mut out = Vec::new();
            // rows of 10 bytes, so the buffer and the marks both grow
            memory_refusal(write_buffered(
                &mut out,
                &count,
                Some(r.test_context().clone()),
                &mut rows(1000, 10, 3, explicit),
            ));
            // every row whose end was recorded is written on and counted
            let lines = out.iter().filter(|&&b| b == b'\n').count() as u64;
            assert!(lines > 0, "{explicit} {left}");
            assert_eq!(count.get(), lines, "{explicit} {left}");
        }
    }
}

#[test]
fn the_charge_is_released_with_the_writer() {
    let r = small_answer();
    let ctx = r.test_context().clone();
    let before = room(&ctx);
    let count = RowCount::new();
    write_buffered(
        &mut Vec::new(),
        &count,
        Some(ctx.clone()),
        &mut rows(1000, 10, 3, false),
    )
    .unwrap();
    assert_eq!(count.get(), 1000);
    assert_eq!(room(&ctx), before);
}
