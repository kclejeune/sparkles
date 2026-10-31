//! The live memory of the block reader of N-Triples streams, counted by a global
//! allocator. This binary holds one test, so no other test allocates beside it.
use sparkles_core::io::{QuadSink, RdfFormat, Source, parse_lines_in_blocks};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

/// Allocations of at least this size are counted: block buffers, not parser state.
const LARGE: usize = 16 << 10;
static LIVE: AtomicIsize = AtomicIsize::new(0);
static PEAK: AtomicIsize = AtomicIsize::new(0);

struct Counting;

fn grew(bytes: usize) {
    if bytes >= LARGE {
        let live = LIVE.fetch_add(bytes as isize, Ordering::SeqCst) + bytes as isize;
        PEAK.fetch_max(live, Ordering::SeqCst);
    }
}

fn shrank(bytes: usize) {
    if bytes >= LARGE {
        LIVE.fetch_sub(bytes as isize, Ordering::SeqCst);
    }
}

// SAFETY: every operation delegates unchanged to System and only updates two atomics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            grew(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        shrank(layout.size());
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, size) };
        if !q.is_null() {
            shrank(layout.size());
            grew(size);
        }
        q
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Slow(usize);

impl QuadSink for Slow {
    fn quad(&mut self, _: oxrdf::Quad) -> sparkles_core::Result<()> {
        self.0 += 1;
        if self.0.is_multiple_of(4) {
            // the parser falls behind the reader
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Ok(())
    }
    fn finish(self) -> sparkles_core::Result<()> {
        Ok(())
    }
}

#[test]
fn block_buffers_and_the_carried_line_stay_within_two_blocks() {
    let block = 64 << 10;
    // lines of 40 to 60 KB, shorter than a block, so that most blocks end in the middle of
    // a long line, padded with the spaces N-Triples allows between terms so that the
    // parser itself allocates nothing large
    let mut data = Vec::new();
    for i in 0..80 {
        let pad = " ".repeat(40_000 + i * 250);
        data.extend_from_slice(format!("<http://e/s{i}>{pad}<http://e/p> \"{i}\" .\n").as_bytes());
    }
    let src = Source::from_reader(std::io::Cursor::new(data), RdfFormat::NTriples, None);
    let base = LIVE.load(Ordering::SeqCst);
    PEAK.store(base, Ordering::SeqCst);
    parse_lines_in_blocks(&src, 1, block, || Slow(0)).unwrap();
    let peak = (PEAK.load(Ordering::SeqCst) - base) as usize;
    eprintln!("peak of large allocations: {peak}");
    assert!(
        peak <= 2 * block,
        "large allocations peaked at {peak}, past two blocks of {block}"
    );
}
