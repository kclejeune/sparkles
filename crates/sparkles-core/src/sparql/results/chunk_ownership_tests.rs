//! Actual current bulk-helper ownership witnesses, independent of writer routing.
use super::*;
use crate::io::{RdfFormat, Source};
use crate::sparql::{QueryOptions, pool::QueryPool, query};
use crate::store::{Store, StoreOptions};

fn base_answer() -> (QueryResult, Arc<QueryPool>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), StoreOptions::default()).unwrap();
    let payload = "x".repeat(64 << 10);
    store
        .load(&[Source::from_bytes(
            format!("<urn:s> <urn:p> \"{payload}\" .").into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let pool = QueryPool::new(16 << 20);
    let result = query(
        store.snapshot(),
        "SELECT ?o WHERE { <urn:s> <urn:p> ?o }",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        result.inner.table.get(0, 0).tag(),
        Tag::Vocab,
        "the real bulk helper must use the actual stored base-vocabulary route"
    );
    drop(store);
    (result, pool, directory)
}

#[test]
fn decoded_chunk_keeps_payload_and_original_credit_after_result_drop() {
    let (result, pool, _directory) = base_answer();
    let cells = Decoded::cells(&result, 0..1, false);
    let decoded = Decoded::new(&result, cells);
    assert_eq!(decoded.terms.len(), 1);
    assert!(matches!(&decoded.terms[0], Term::Literal(l) if l.value().len() == 64 << 10));
    let original = Arc::downgrade(&result.inner.ctx);
    drop(result);
    assert!(
        pool.granted() >= 64 << 10,
        "the actual bulk decoded 64KiB payload needs its original live credit; live={}",
        pool.granted()
    );
    assert!(original.upgrade().is_some());
    assert!(matches!(&decoded.terms[0], Term::Literal(l) if l.value().len() == 64 << 10));
    drop(decoded);
    assert!(original.upgrade().is_none());
    assert_eq!(pool.granted(), 0);
}

#[test]
fn prefetched_cells_keep_pair_storage_and_original_credit_until_final_drop() {
    let (result, pool, _directory) = base_answer();
    let before = result.inner.ctx.live_bytes();
    let cells = Decoded::cells(&result, 0..1, false);
    assert_eq!(cells.pairs.len(), 1);
    let needed = (cells.pairs.capacity() * std::mem::size_of::<(u64, u32)>()) as u64;
    assert!(
        result.inner.ctx.live_bytes() >= before + needed,
        "actual pair storage must be admitted before allocation; before={before}, after={}, needed={needed}",
        result.inner.ctx.live_bytes()
    );
    let original = Arc::downgrade(&result.inner.ctx);
    drop(result);
    assert!(original.upgrade().is_some());
    assert!(pool.granted() >= needed);
    assert_eq!(cells.pairs.len(), 1);
    drop(cells);
    assert!(original.upgrade().is_none());
    assert_eq!(pool.granted(), 0);
}

#[test]
fn chunk_pair_refusal_precedes_allocation_and_preserves_nonempty_source() {
    use crate::algo::memory::probe;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (result, pool, _directory) = base_answer();
    let pair = std::mem::size_of::<(u64, u32)>() as u64;
    let pressure = result
        .inner
        .ctx
        .charge(result.inner.ctx.memory_remaining() - pair + 1)
        .unwrap();
    let before = result.inner.ctx.live_bytes();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let hook = probe::install(
        move |point| {
            if matches!(point, probe::Point::Allocation("chunk-pairs", _)) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        },
        None,
    );
    let error = Decoded::try_cells(&result, 0..1, false)
        .err()
        .expect("fixed local pair refusal");
    assert!(
        matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory && b.limit == 4 << 20 && b.requested == b.limit + 1)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.inner.ctx.live_bytes(), before);
    assert_eq!(result.inner.table.get(0, 0).tag(), Tag::Vocab);
    drop(hook);
    drop(pressure);
    let decoded =
        Decoded::try_new(&result, Decoded::try_cells(&result, 0..1, false).unwrap()).unwrap();
    assert!(matches!(&decoded.terms[0], Term::Literal(l) if l.value().len() == 64 << 10));
    drop(decoded);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn chunk_allocation_faults_retire_partials_and_keep_original_values() {
    use crate::algo::memory::probe;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (result, pool, _directory) = base_answer();
    let before = result.inner.ctx.live_bytes();
    for site in [
        "chunk-pairs",
        "chunk-distinct-ids",
        "chunk-slots",
        "chunk-term-container",
        "chunk-final-terms",
        "chunk-row",
        "chunk-term",
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let hook = probe::install(
            move |point| {
                if let probe::Point::Allocation(name, _) = point
                    && name == site
                {
                    observed.fetch_add(1, Ordering::SeqCst);
                }
            },
            Some(site),
        );
        let attempt = (|| -> Result<()> {
            let cells = Decoded::try_cells(&result, 0..1, false)?;
            let decoded = Decoded::try_new(&result, cells)?;
            let _row = ChunkRow::decode(&result, &decoded, 0)?;
            Ok(())
        })();
        assert!(matches!(attempt, Err(Error::Invalid(_))), "{site}");
        assert!(
            calls.load(Ordering::SeqCst) > 0,
            "actual allocation callback {site}"
        );
        assert_eq!(
            result.inner.ctx.live_bytes(),
            before,
            "partial retirement {site}"
        );
        drop(hook);
    }
    let decoded =
        Decoded::try_new(&result, Decoded::try_cells(&result, 0..1, false).unwrap()).unwrap();
    assert!(matches!(&decoded.terms[0], Term::Literal(l) if l.value().len() == 64 << 10));
    drop(decoded);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn bulk_cells_and_rows_refuse_another_actual_context_in_the_same_pool() {
    let (result, pool, _directory) = base_answer();
    let other = query(
        result.inner.ctx.snap.clone(),
        "SELECT ?o WHERE { <urn:s> <urn:p> ?o }",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let before = other.inner.ctx.live_bytes();
    let cells = Decoded::try_cells(&result, 0..1, false).unwrap();
    assert!(matches!(
        Decoded::try_new(&other, cells),
        Err(Error::Invalid(_))
    ));
    assert_eq!(other.inner.ctx.live_bytes(), before);
    let decoded =
        Decoded::try_new(&result, Decoded::try_cells(&result, 0..1, false).unwrap()).unwrap();
    assert!(matches!(
        ChunkRow::decode(&other, &decoded, 0),
        Err(Error::Invalid(_))
    ));
    assert_eq!(other.inner.ctx.live_bytes(), before);
    drop(decoded);
    drop(other);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

fn large_answer() -> (QueryResult, Arc<QueryPool>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), StoreOptions::default()).unwrap();
    store
        .load(&[Source::from_bytes(
            b"<urn:s0> <urn:p> \"small\" .".to_vec(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let snapshot = store.snapshot();
    let predicate = snapshot.lookup_iri("urn:p").unwrap();
    let object = snapshot
        .lookup_term(&oxrdf::Literal::new_simple_literal("small").into())
        .unwrap();
    drop(snapshot);
    let mut write = store.write();
    for i in 1..=DECODE_ROWS {
        let subject = write
            .intern(&oxrdf::NamedNode::new_unchecked(format!("urn:s{i}")).into())
            .unwrap();
        write
            .insert([subject, predicate, object, Id::DEFAULT_GRAPH])
            .unwrap();
    }
    write.commit().unwrap();
    let pool = QueryPool::new(64 << 20);
    let result = query(
        store.snapshot(),
        "SELECT ?o WHERE { ?s <urn:p> ?o }",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(16 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.len(), DECODE_ROWS + 1);
    assert!(
        result.inner.table.cols[0]
            .iter()
            .all(|id| id.tag() == Tag::Vocab)
    );
    drop(store);
    (result, pool, directory)
}

#[test]
fn public_large_writer_keeps_threshold_values_counts_and_joins_prefetch() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Sink {
        bytes: usize,
        prefix: Vec<u8>,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.prefix.extend_from_slice(
                &bytes[..bytes.len().min(16usize.saturating_sub(self.prefix.len()))],
            );
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (result, pool, _directory) = large_answer();
    let starts = Arc::new(AtomicUsize::new(0));
    let ends = Arc::new(AtomicUsize::new(0));
    let a = starts.clone();
    let b = ends.clone();
    *result.inner.ctx.chunk_hook.lock() = Some(Arc::new(move |stage, end| {
        if stage == "prefetch-start" {
            assert_eq!(end, DECODE_ROWS);
            a.fetch_add(1, Ordering::SeqCst);
        }
        if stage == "prefetch-end" {
            assert_eq!(end, DECODE_ROWS);
            b.fetch_add(1, Ordering::SeqCst);
        }
    }));
    let mut sink = Sink {
        bytes: 0,
        prefix: Vec::new(),
    };
    let sent = Cell::new(0);
    write_solutions_counted(&result, SolutionsFormat::Tsv, &mut sink, None, &sent).unwrap();
    assert_eq!(sent.get(), (DECODE_ROWS + 1) as u64);
    assert!(sink.prefix.starts_with(b"?o\n\"small\"\n"));
    assert_eq!(sink.bytes, 3 + 8 * (DECODE_ROWS + 1));
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(ends.load(Ordering::SeqCst), 1);
    let hook = result.inner.ctx.chunk_hook.lock().take();
    drop(hook);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn bulk_parallel_payloads_are_owned_and_joined_with_one_and_eight_threads() {
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    for threads in [1, 8] {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path(), StoreOptions::default()).unwrap();
        let n = PAR_DECODE_MIN + 1;
        let mut text = String::new();
        for i in 0..n {
            text.push_str(&format!("<urn:s{i}> <urn:p> \"v{i:05}\" .\n"));
        }
        store
            .load(&[Source::from_bytes(
                text.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let pool = QueryPool::new(64 << 20);
        let result = query(
            store.snapshot(),
            "SELECT ?o WHERE { ?s <urn:p> ?o }",
            &QueryOptions {
                pool: Some(pool.clone()),
                max_memory_bytes: Some(16 << 20),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(result.len(), n);
        let starts = Arc::new(AtomicUsize::new(0));
        let ends = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let rendezvous = Arc::new(Barrier::new(2));
        let (a, b, c, d, e) = (
            starts.clone(),
            ends.clone(),
            active.clone(),
            maximum.clone(),
            rendezvous.clone(),
        );
        *result.inner.ctx.chunk_hook.lock() = Some(Arc::new(move |stage, _| {
            if stage == "decode-start" {
                let ordinal = a.fetch_add(1, Ordering::SeqCst);
                let current = c.fetch_add(1, Ordering::SeqCst) + 1;
                d.fetch_max(current, Ordering::SeqCst);
                if threads == 8 && ordinal < 2 {
                    e.wait();
                }
            }
            if stage == "decode-end" {
                b.fetch_add(1, Ordering::SeqCst);
                c.fetch_sub(1, Ordering::SeqCst);
            }
        }));
        let executor = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let decoded = executor.install(|| {
            Decoded::try_new(&result, Decoded::try_cells(&result, 0..n, false).unwrap()).unwrap()
        });
        assert_eq!(decoded.terms.len(), n);
        for (i, term) in decoded.terms.iter().enumerate() {
            assert!(matches!(term, Term::Literal(l) if l.value() == format!("v{i:05}")));
        }
        assert_eq!(starts.load(Ordering::SeqCst), n.div_ceil(DECODE_TASK));
        assert_eq!(ends.load(Ordering::SeqCst), starts.load(Ordering::SeqCst));
        assert_eq!(active.load(Ordering::SeqCst), 0);
        if threads == 8 {
            assert!(maximum.load(Ordering::SeqCst) >= 2);
        }
        let hook = result.inner.ctx.chunk_hook.lock().take();
        drop(hook);
        let original = Arc::downgrade(&result.inner.ctx);
        drop(result);
        drop(store);
        drop(executor);
        assert!(original.upgrade().is_some());
        assert!(pool.granted() > 0);
        drop(decoded);
        assert!(original.upgrade().is_none());
        assert_eq!(pool.granted(), 0);
    }
}

#[test]
fn scoped_prefetch_panic_is_an_error_without_unchecked_redo() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (result, pool, _directory) = large_answer();
    let starts = Arc::new(AtomicUsize::new(0));
    let ends = Arc::new(AtomicUsize::new(0));
    let decodes = Arc::new(AtomicUsize::new(0));
    let (a, b, c) = (starts.clone(), ends.clone(), decodes.clone());
    *result.inner.ctx.chunk_hook.lock() = Some(Arc::new(move |stage, _| {
        if stage == "decode-start" {
            c.fetch_add(1, Ordering::SeqCst);
        }
        if stage == "prefetch-end" {
            b.fetch_add(1, Ordering::SeqCst);
        }
        if stage == "prefetch-start" {
            a.fetch_add(1, Ordering::SeqCst);
            panic!("actual owned prefetch fault");
        }
    }));
    let sent = Cell::new(0);
    let error =
        write_solutions_counted(&result, SolutionsFormat::Tsv, std::io::sink(), None, &sent)
            .unwrap_err();
    assert!(
        matches!(error, Error::Invalid(message) if message == "query output prefetch worker panicked")
    );
    assert!(sent.get() <= DECODE_ROWS as u64);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(ends.load(Ordering::SeqCst), 1);
    assert_eq!(
        decodes.load(Ordering::SeqCst),
        1,
        "no fallback or next-chunk decoding after panic"
    );
    let hook = result.inner.ctx.chunk_hook.lock().take();
    drop(hook);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn observed_io_error_stays_first_while_later_prefetch_panic_is_joined() {
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    struct Broken {
        header: usize,
    }
    impl Write for Broken {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            // Accept an independently flushed header too; the failure belongs to
            // row output after the real prefetch worker has been started.
            if self.header < 3 {
                let n = (3 - self.header).min(bytes.len());
                self.header += n;
                return Ok(n);
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "actual chosen writer failure",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (result, pool, _directory) = large_answer();
    let rendezvous = Arc::new(Barrier::new(2));
    let starts = Arc::new(AtomicUsize::new(0));
    let ends = Arc::new(AtomicUsize::new(0));
    let ios = Arc::new(AtomicUsize::new(0));
    let (a, b, c, d) = (
        rendezvous.clone(),
        starts.clone(),
        ends.clone(),
        ios.clone(),
    );
    *result.inner.ctx.chunk_hook.lock() = Some(Arc::new(move |stage, _| {
        if stage == "prefetch-end" {
            c.fetch_add(1, Ordering::SeqCst);
        }
        if stage == "prefetch-start" {
            b.fetch_add(1, Ordering::SeqCst);
            a.wait();
            panic!("secondary actual prefetch fault after IO selection");
        }
        if stage == "io-failure-recorded" {
            d.fetch_add(1, Ordering::SeqCst);
            a.wait();
        }
    }));
    let sent = Cell::new(0);
    let error = write_solutions_counted(
        &result,
        SolutionsFormat::Tsv,
        Broken { header: 0 },
        None,
        &sent,
    )
    .unwrap_err();
    assert!(
        matches!(error, Error::Io(e) if e.kind() == std::io::ErrorKind::BrokenPipe && e.to_string() == "actual chosen writer failure")
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(ends.load(Ordering::SeqCst), 1);
    assert_eq!(
        ios.load(Ordering::SeqCst),
        1,
        "real IO selection callback must run"
    );
    let hook = result.inner.ctx.chunk_hook.lock().take();
    drop(hook);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn chunk_cancel_before_copy_and_after_last_key_returns_all_partial_credit() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for after_key in [false, true] {
        let (result, pool, _directory) = base_answer();
        let before = result.inner.ctx.live_bytes();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let flag = result.inner.ctx.cancel.clone();
        *result.inner.ctx.chunk_hook.lock() = Some(Arc::new(move |stage, _| {
            if stage
                == if after_key {
                    "term-copied"
                } else {
                    "decode-start"
                }
            {
                observed.fetch_add(1, Ordering::SeqCst);
                flag.store(true, Ordering::SeqCst);
            }
        }));
        let cells = Decoded::try_cells(&result, 0..1, false).unwrap();
        assert!(matches!(
            Decoded::try_new(&result, cells),
            Err(Error::Cancelled)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.inner.ctx.live_bytes(), before);
        let hook = result.inner.ctx.chunk_hook.lock().take();
        drop(hook);
        drop(result);
        assert_eq!(pool.granted(), 0);
    }
}

#[test]
fn bulk_nested_terms_decode_and_retire_on_their_protected_stack() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), StoreOptions::default()).unwrap();
    let levels = crate::nesting::MAX_TRIPLE_TERMS.min(48);
    let text = crate::sparql::depth::with_stack(levels, || {
        let mut term: Term = oxrdf::Literal::new_simple_literal("leaf").into();
        for _ in 0..levels {
            term = Term::Triple(Box::new(oxrdf::Triple::new(
                oxrdf::NamedNode::new_unchecked("urn:a"),
                oxrdf::NamedNode::new_unchecked("urn:b"),
                term,
            )));
        }
        format!("<urn:s> <urn:p> {term} .")
    });
    store
        .load(&[Source::from_bytes(
            text.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let pool = QueryPool::new(16 << 20);
    let result = query(
        store.snapshot(),
        "SELECT ?o WHERE { <urn:s> <urn:p> ?o }",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let decoded =
        Decoded::try_new(&result, Decoded::try_cells(&result, 0..1, false).unwrap()).unwrap();
    assert_eq!(crate::sparql::output::term_depth(&decoded.terms[0]), levels);
    let original = Arc::downgrade(&result.inner.ctx);
    drop(result);
    drop(store);
    assert!(original.upgrade().is_some());
    drop(decoded);
    assert!(original.upgrade().is_none());
    assert_eq!(pool.granted(), 0);
}

#[test]
fn missing_sorted_suffix_refuses_after_a_real_prefix_and_retires_it() {
    let (result, pool, _directory) = base_answer();
    let ctx = &result.inner.ctx;
    let container = ctx
        .charge(2 * std::mem::size_of::<Term>() as u64)
        .unwrap()
        .into_output(ctx);
    let before = ctx.live_bytes();
    let copied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = copied.clone();
    *ctx.chunk_hook.lock() = Some(Arc::new(move |stage, _| {
        if stage == "term-copied" {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    let valid = result.inner.table.get(0, 0).payload();
    let error = Decoded::decode_part(&result, &[valid, (1 << 60) - 1], &container, false)
        .err()
        .expect("required missing sorted suffix must refuse");
    assert!(
        matches!(error, Error::Corrupt(message) if message == "missing vocabulary id in output chunk")
    );
    assert_eq!(copied.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(ctx.live_bytes(), before, "actual accepted prefix retired");
    drop(ctx.chunk_hook.lock().take());
    drop(container);
    let decoded =
        Decoded::try_new(&result, Decoded::try_cells(&result, 0..1, false).unwrap()).unwrap();
    assert!(matches!(&decoded.terms[0], Term::Literal(l) if l.value().len() == 64 << 10));
    drop(decoded);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn malformed_structured_key_refuses_before_the_actual_term_copy() {
    let (result, pool, _directory) = base_answer();
    let before = result.inner.ctx.live_bytes();
    let payload = result.inner.ctx.charge(0).unwrap();
    for key in [&b"("[..], &b"(\x80"[..], &b"(\x03x"[..]] {
        assert!(matches!(
            result.inner.ctx.output_key(key, &payload),
            Err(Error::Corrupt(_))
        ));
        assert_eq!(payload.bytes(), 0);
        assert_eq!(result.inner.ctx.live_bytes(), before);
    }
    drop(payload);
    drop(result);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn second_payload_refusal_retires_the_real_nonempty_decoded_prefix() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), StoreOptions::default()).unwrap();
    store
        .load(&[Source::from_bytes(
            format!(
                "<urn:a> <urn:p> \"{}\" .\n<urn:b> <urn:p> \"{}\" .",
                "a".repeat(64 << 10),
                "b".repeat(64 << 10)
            )
            .into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let pool = QueryPool::new(16 << 20);
    let result = query(
        store.snapshot(),
        "SELECT ?o WHERE { ?s <urn:p> ?o }",
        &QueryOptions {
            pool: Some(pool.clone()),
            max_memory_bytes: Some(4 << 20),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 2);
    let mut ids = [result.inner.table.get(0, 0), result.inner.table.get(1, 0)];
    assert!(ids.iter().all(|id| id.tag() == Tag::Vocab));
    ids.sort_unstable_by_key(|id| id.payload());
    let ctx = &result.inner.ctx;
    let container = ctx
        .charge(2 * std::mem::size_of::<Term>() as u64)
        .unwrap()
        .into_output(ctx);
    // The fixed 200KiB remainder fits one 64KiB decoded payload plus old/new
    // reconstruction capacity, but refuses the second simultaneously live copy.
    let pressure = ctx.charge(ctx.memory_remaining() - (200 << 10)).unwrap();
    let before = ctx.live_bytes();
    let copies = Arc::new(AtomicUsize::new(0));
    let observed = copies.clone();
    *ctx.chunk_hook.lock() = Some(Arc::new(move |stage, _| {
        if stage == "term-copied" {
            observed.fetch_add(1, Ordering::SeqCst);
        }
    }));
    let error = Decoded::decode_part(&result, &ids.map(Id::payload), &container, false)
        .err()
        .expect("second physical copy must refuse");
    assert!(
        matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory && b.limit == 4 << 20 && b.requested > b.limit)
    );
    assert_eq!(
        copies.load(Ordering::SeqCst),
        1,
        "real first payload existed before refusal"
    );
    assert_eq!(
        ctx.live_bytes(),
        before,
        "all partial payload and reconstruction storage retired"
    );
    drop(ctx.chunk_hook.lock().take());
    drop(pressure);
    drop(container);
    let decoded =
        Decoded::try_new(&result, Decoded::try_cells(&result, 0..2, false).unwrap()).unwrap();
    assert!(matches!(&decoded.terms[0], Term::Literal(l) if l.value() == "a".repeat(64 << 10)));
    assert!(matches!(&decoded.terms[1], Term::Literal(l) if l.value() == "b".repeat(64 << 10)));
    drop(decoded);
    drop(result);
    drop(store);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn bulk_rows_preserve_real_base_delta_local_inline_and_unbound_cells() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), StoreOptions::default()).unwrap();
    store
        .load(&[Source::from_bytes(
            b"<urn:base> <urn:p> \"base\" .".to_vec(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let predicate = store.snapshot().lookup_iri("urn:p").unwrap();
    let mut write = store.write();
    let subject = write
        .intern(&oxrdf::NamedNode::new_unchecked("urn:delta").into())
        .unwrap();
    let object = write
        .intern(&oxrdf::Literal::new_simple_literal("delta").into())
        .unwrap();
    assert_eq!(object.tag(), Tag::Delta);
    write
        .insert([subject, predicate, object, Id::DEFAULT_GRAPH])
        .unwrap();
    write.commit().unwrap();
    let pool = QueryPool::new(16 << 20);
    let result = query(store.snapshot(),
        "SELECT ?base ?delta (\"query-local\" AS ?local) (7 AS ?inline) ?missing WHERE { <urn:base> <urn:p> ?base . <urn:delta> <urn:p> ?delta }",
        &QueryOptions { pool: Some(pool.clone()), max_memory_bytes: Some(4 << 20), no_cache: true, ..Default::default() },
    ).unwrap();
    assert_eq!(result.len(), 1);
    for (column, tag) in [Tag::Vocab, Tag::Delta, Tag::Local, Tag::Int, Tag::Undef]
        .into_iter()
        .enumerate()
    {
        assert_eq!(result.inner.table.get(0, column).tag(), tag);
    }
    let decoded =
        Decoded::try_new(&result, Decoded::try_cells(&result, 0..1, false).unwrap()).unwrap();
    assert_eq!(decoded.terms.len(), 1);
    let before = result.inner.ctx.live_bytes();
    let row = ChunkRow::decode(&result, &decoded, 0).unwrap();
    assert_eq!(row.terms.len(), 4);
    for (column, expected) in ["base", "delta", "query-local", "7"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(row.terms[column].0, column);
        assert!(matches!(row.terms[column].1.as_ref(), Term::Literal(l) if l.value() == expected));
    }
    assert!(matches!(row.terms[0].1, Cow::Borrowed(_)));
    assert!(
        row.terms[1..]
            .iter()
            .all(|(_, term)| matches!(term, Cow::Owned(_)))
    );
    assert!(result.inner.ctx.live_bytes() > before);
    drop(row);
    assert_eq!(result.inner.ctx.live_bytes(), before);
    drop(decoded);
    drop(result);
    drop(store);
    assert_eq!(pool.granted(), 0);
}

fn caller_unwind_packet(completed_cells: bool) {
    use std::sync::{
        Barrier,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    };
    struct CallerPanic {
        header: usize,
        ready: mpsc::Receiver<()>,
    }
    impl Write for CallerPanic {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.header < 3 {
                let n = (3 - self.header).min(bytes.len());
                self.header += n;
                return Ok(n);
            }
            self.ready
                .recv()
                .expect("actual owned worker reached its selected stage");
            std::panic::panic_any("original supplied writer panic");
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (result, pool, _directory) = large_answer();
    let baseline = result.inner.ctx.live_bytes();
    let (ready, receive) = mpsc::channel();
    let barrier = Arc::new(Barrier::new(2));
    let starts = Arc::new(AtomicUsize::new(0));
    let ends = Arc::new(AtomicUsize::new(0));
    let unwinds = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicU64::new(u64::MAX));
    let weak = Arc::downgrade(&result.inner.ctx);
    let (a, b, c, d, e) = (
        starts.clone(),
        ends.clone(),
        unwinds.clone(),
        observed.clone(),
        barrier.clone(),
    );
    *result.inner.ctx.chunk_hook.lock() = Some(Arc::new(move |stage, _| {
        if stage == "prefetch-start" {
            a.fetch_add(1, Ordering::SeqCst);
            if !completed_cells {
                let _ = ready.send(());
                e.wait();
            }
        }
        if stage == "prefetch-end" {
            if completed_cells {
                let _ = ready.send(());
                e.wait();
            }
            b.fetch_add(1, Ordering::SeqCst);
        }
        if stage == "caller-scope-unwind" {
            c.fetch_add(1, Ordering::SeqCst);
            if let Some(ctx) = weak.upgrade() {
                d.store(ctx.live_bytes(), Ordering::SeqCst);
            }
            if a.load(Ordering::SeqCst) != 0 {
                e.wait();
            }
        }
    }));
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let sent = Cell::new(0);
        write_solutions_counted(
            &result,
            SolutionsFormat::Tsv,
            CallerPanic {
                header: 0,
                ready: receive,
            },
            None,
            &sent,
        )
    }))
    .expect_err("the supplied writer panic must remain a panic");
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"original supplied writer panic")
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(
        ends.load(Ordering::SeqCst),
        1,
        "scope has joined the real worker before returning"
    );
    assert_eq!(
        unwinds.load(Ordering::SeqCst),
        1,
        "outside-observed unwind callback actually ran"
    );
    let during_unwind = observed.load(Ordering::SeqCst);
    assert_eq!(
        result.inner.ctx.live_bytes(),
        baseline,
        "partials retired after automatic join"
    );
    assert_eq!(result.len(), DECODE_ROWS + 1);
    assert!(
        matches!(result.inner.ctx.snap.term(result.inner.table.get(0,0)), Some(Term::Literal(l)) if l.value() == "small")
    );
    drop(result.inner.ctx.chunk_hook.lock().take());
    drop(result);
    assert_eq!(pool.granted(), 0);
    let returned_cells = if completed_cells {
        std::mem::size_of::<(u64, u32)>() as u64
    } else {
        0
    };
    assert_eq!(
        during_unwind,
        baseline + 4096 + 4096 + returned_cells,
        "packet contribution returned before actual scope join; completed_cells={completed_cells}"
    );
}

#[test]
fn packet_credit_survives_caller_unwind_blocked_prefetch() {
    caller_unwind_packet(false);
}

#[test]
fn packet_credit_survives_caller_unwind_completed_prefetch() {
    caller_unwind_packet(true);
}
