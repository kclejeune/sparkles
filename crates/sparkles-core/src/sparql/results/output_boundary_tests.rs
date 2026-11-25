//! Real existing eager-writer boundaries; no canonical hierarchy activation.
use super::*;
use crate::sparql::{QueryOptions, pool::QueryPool, query};
use crate::store::{Store, StoreOptions};

const MB: u64 = 1 << 20;
const LONG: usize = OUTPUT_BUFFER * 2;

#[derive(Clone, Copy)]
enum Format {
    Native,
    Tsv,
}

impl Format {
    fn write(self, r: &QueryResult, out: impl Write, sent: &RowCount) -> Result<()> {
        match self {
            Self::Native => write_solutions_rows(r, SolutionsFormat::Sparkles, out, None, sent),
            Self::Tsv => write_solutions_rows(r, SolutionsFormat::Tsv, out, None, sent),
        }
    }
}

fn answer(shared: bool) -> QueryResult {
    let store = Store::in_memory(StoreOptions::default());
    let options = QueryOptions {
        max_memory_bytes: Some(if shared { 32 * MB } else { MB }),
        pool: shared.then(|| QueryPool::new(8 * MB)),
        no_cache: true,
        ..Default::default()
    };
    let text = format!(
        "SELECT ?o WHERE {{ VALUES ?o {{ \"{}\" \"tail\" }} }}",
        "x".repeat(LONG)
    );
    let r = query(store.snapshot(), &text, &options).unwrap();
    assert_eq!((r.len(), r.test_table().width()), (2, 1));
    // Establish that both actual local terms fit, rather than mistake setup refusal
    // or a genuine UNDEF for the writer defect.
    let values = r.test_application_rows();
    assert_eq!(values.len(), 2);
    assert!(matches!(&values[0][0], Some(Term::Literal(v)) if v.value() == "x".repeat(LONG)));
    assert!(matches!(&values[1][0], Some(Term::Literal(v)) if v.value() == "tail"));
    r.test_context().check_memory_failure().unwrap();
    r
}

fn refuse(r: &QueryResult, shared: bool) -> Budget {
    let bytes = if shared { 16 * MB } else { 2 * MB };
    let error = match r.test_context().charge(bytes) {
        Err(error) => error,
        Ok(_) => panic!("the fixed charge must exceed the real fixture allowance"),
    };
    let Error::BudgetExceeded(budget) = error else {
        panic!("unexpected refusal origin: {error:?}");
    };
    assert_eq!(
        budget.kind,
        if shared {
            BudgetKind::ServerMemory
        } else {
            BudgetKind::Memory
        }
    );
    assert_eq!(budget.limit, if shared { 8 * MB } else { MB });
    assert!(budget.requested >= bytes);
    r.test_context().fail_memory(&Error::BudgetExceeded(budget));
    assert!(
        matches!(r.test_context().check_memory_failure(), Err(Error::BudgetExceeded(b)) if b == budget)
    );
    budget
}

fn exact_refusal(result: Result<()>, expected: Budget) {
    match result {
        Err(Error::BudgetExceeded(actual)) => assert_eq!(actual, expected),
        other => panic!("writer lost the original typed refusal {expected:?}: {other:?}"),
    }
}

#[test]
fn positive_writers_preserve_bound_cells_and_whole_row_counts() {
    for shared in [false, true] {
        let r = answer(shared);
        let sent = RowCount::new();
        let mut native = Vec::new();
        Format::Native.write(&r, &mut native, &sent).unwrap();
        assert_eq!(sent.get(), 2);
        let value: J = serde_json::from_slice(&native).unwrap();
        assert_eq!(value["queryType"], "SELECT");
        assert_eq!(value["vars"], serde_json::json!(["o"]));
        assert_eq!(
            value["rows"],
            serde_json::json!([
                [{"type":"literal","value":"x".repeat(LONG)}],
                [{"type":"literal","value":"tail"}]
            ])
        );
        assert_eq!(value["meta"]["sentRows"], 2);
        let mut tsv = Vec::new();
        let sent = RowCount::new();
        Format::Tsv.write(&r, &mut tsv, &sent).unwrap();
        assert_eq!(sent.get(), 2);
        assert_eq!(
            tsv,
            format!("?o\n\"{}\"\n\"tail\"\n", "x".repeat(LONG)).as_bytes()
        );
    }
}

fn before_prefix(format: Format, shared: bool) {
    let r = answer(shared);
    let expected = refuse(&r, shared);
    let mut out = Vec::new();
    let sent = RowCount::new();
    exact_refusal(format.write(&r, &mut out, &sent), expected);
    assert_eq!(sent.get(), 0);
    assert!(!out.windows(4).any(|b| b == b"null"));
    assert!(!out.windows(6).any(|b| b == b"\"meta\""));
}

#[test]
fn native_before_prefix_preserves_local_refusal() {
    before_prefix(Format::Native, false);
}

#[test]
fn native_before_prefix_preserves_work_refusal() {
    before_prefix(Format::Native, true);
}

#[test]
fn tsv_before_prefix_preserves_local_refusal() {
    before_prefix(Format::Tsv, false);
}

#[test]
fn tsv_before_prefix_preserves_work_refusal() {
    before_prefix(Format::Tsv, true);
}

struct RefuseAfterPrefix<'a> {
    r: &'a QueryResult,
    shared: bool,
    observed: Option<Budget>,
    out: Vec<u8>,
}

impl Write for RefuseAfterPrefix<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.out.extend_from_slice(bytes);
        if self.observed.is_none() {
            assert!(!self.out.is_empty());
            self.observed = Some(refuse(self.r, self.shared));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn after_prefix(format: Format, shared: bool) {
    let r = answer(shared);
    let mut out = RefuseAfterPrefix {
        r: &r,
        shared,
        observed: None,
        out: Vec::new(),
    };
    let sent = RowCount::new();
    let result = format.write(&r, &mut out, &sent);
    let expected = out
        .observed
        .expect("large first row must flush before decoding the next row");
    exact_refusal(result, expected);
    assert_eq!(sent.get(), 1, "only the accepted complete first row counts");
    assert!(!out.out.windows(4).any(|b| b == b"null"));
    assert!(!out.out.windows(4).any(|b| b == b"tail"));
    match format {
        Format::Native => {
            assert!(serde_json::from_slice::<J>(&out.out).is_err());
            assert!(!out.out.windows(6).any(|b| b == b"\"meta\""));
        }
        Format::Tsv => assert_eq!(
            out.out,
            format!("?o\n\"{}\"\n", "x".repeat(LONG)).as_bytes()
        ),
    }
}

#[test]
fn native_after_prefix_preserves_local_refusal() {
    after_prefix(Format::Native, false);
}

#[test]
fn native_after_prefix_preserves_work_refusal() {
    after_prefix(Format::Native, true);
}

#[test]
fn tsv_after_prefix_preserves_local_refusal() {
    after_prefix(Format::Tsv, false);
}

#[test]
fn tsv_after_prefix_preserves_work_refusal() {
    after_prefix(Format::Tsv, true);
}

struct FailIo;
impl Write for FailIo {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "first physical write",
        ))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn an_actual_io_failure_keeps_its_kind_and_zero_accepted_rows() {
    for format in [Format::Native, Format::Tsv] {
        let r = answer(false);
        let sent = RowCount::new();
        assert!(
            matches!(format.write(&r, FailIo, &sent), Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe)
        );
        assert_eq!(sent.get(), 0);
        r.test_context().check_memory_failure().unwrap();
    }
}

#[test]
fn a_private_row_keeps_real_credit_until_its_payload_is_dropped() {
    for shared in [false, true] {
        let r = answer(shared);
        let before = r.test_context().live_bytes();
        let row = output::ScratchRow::decode(&r, 0).unwrap();
        assert!(matches!(&row.terms[0], Some(Term::Literal(v)) if v.value().len() == LONG));
        assert!(r.test_context().live_bytes() >= before + LONG as u64);
        drop(row);
        assert_eq!(r.test_context().live_bytes(), before);
        r.test_context().check_memory_failure().unwrap();
    }
}

#[test]
fn one_cell_fits_but_the_complete_row_refuses_before_publication_and_recovers() {
    let store = Store::in_memory(StoreOptions::default());
    let text = format!(
        "SELECT ?a ?b WHERE {{ VALUES (?a ?b) {{ (\"{}\" \"{}\") }} }}",
        "x".repeat(LONG),
        "x".repeat(LONG)
    );
    let r = query(
        store.snapshot(),
        &text,
        &QueryOptions {
            no_cache: true,
            max_memory_bytes: Some(MB),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((r.len(), r.test_table().width()), (1, 2));
    let before = r.test_context().live_bytes();
    let left = LONG as u64 + 512;
    let blocker = r
        .test_context()
        .charge(MB.checked_sub(before + left).unwrap())
        .unwrap();
    let blocked = r.test_context().live_bytes();
    assert_eq!(blocked, MB - left);
    // A single actual decoded cell fits, including its owned string. The two
    // independent cells together exceed this same fixed remaining allowance.
    let cell_credit = crate::sparql::ctx::RowCredit::new(r.test_context());
    let key_credit = crate::sparql::ctx::RowCredit::new(r.test_context());
    let cell = r
        .test_context()
        .output_scratch_term(
            r.test_table().cols[0][0],
            &cell_credit,
            &key_credit,
            &mut Vec::new(),
        )
        .unwrap();
    assert!(matches!(&cell, Some(Term::Literal(v)) if v.value().len() == LONG));
    drop(cell);
    drop(cell_credit);
    drop(key_credit);
    assert_eq!(r.test_context().live_bytes(), blocked);
    for format in [Format::Native, Format::Tsv] {
        let mut out = Vec::new();
        let sent = RowCount::new();
        let error = format.write(&r, &mut out, &sent).unwrap_err();
        assert!(
            matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory && b.limit == MB && b.requested > MB)
        );
        assert_eq!(sent.get(), 0);
        assert!(
            !out.windows(32)
                .any(|part| part == b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx")
        );
        assert_eq!(
            r.test_context().live_bytes(),
            blocked,
            "failed row returned only its own credit"
        );
        r.test_context().check_memory_failure().unwrap();
    }
    drop(blocker);
    assert_eq!(r.test_context().live_bytes(), before);
    for format in [Format::Native, Format::Tsv] {
        let sent = RowCount::new();
        format.write(&r, Vec::new(), &sent).unwrap();
        assert_eq!(sent.get(), 1);
        assert_eq!(r.test_context().live_bytes(), before);
    }
}

fn classifier_late_cap(shared: bool) {
    let r = answer(shared);
    let expected = Cell::new(None);
    let mut limited = LimitedWriter::new(Vec::new(), Some(0), None);
    let sent = RowCount::new();
    let error = write_buffered(&mut limited, &sent, None, &mut |out, _| {
        out.write_all(b"prefix").map_err(io)?;
        let budget = refuse(&r, shared);
        expected.set(Some(budget));
        Err(Error::BudgetExceeded(budget))
    })
    .unwrap_err();
    let expected = expected
        .get()
        .expect("the real charge refusal preceded the drain");
    assert!(matches!(&error, Error::BudgetExceeded(b) if *b == expected));
    assert_eq!(
        limited.exceeded,
        Some(6),
        "only the later error drain hit the byte cap"
    );
    assert!(limited.get_ref().is_empty());
    assert_eq!((limited.written(), sent.get()), (0, 0));
    exact_refusal(Err(limited.classify(error)), expected);
}

#[test]
fn classifier_preserves_local_refusal_before_later_byte_cap() {
    classifier_late_cap(false);
}

#[test]
fn classifier_preserves_work_refusal_before_later_byte_cap() {
    classifier_late_cap(true);
}

#[test]
fn classifier_preserves_memory_refusal_before_later_cancellation() {
    let r = answer(false);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut limited = LimitedWriter::new(Vec::new(), None, Some(cancel.clone()));
    // Leave exactly1KiB before the next normal64KiB cancellation observation.
    let prior = CANCEL_CHECK_BYTES as usize - 1024;
    limited.write_all(&vec![b' '; prior]).unwrap();
    let expected = Cell::new(None);
    let sent = RowCount::new();
    let error = write_buffered(&mut limited, &sent, None, &mut |out, _| {
        out.write_all(&[b'x'; 1024]).map_err(io)?;
        let budget = refuse(&r, false);
        expected.set(Some(budget));
        cancel.store(true, Ordering::Relaxed);
        Err(Error::BudgetExceeded(budget))
    })
    .unwrap_err();
    let expected = expected.get().unwrap();
    assert!(matches!(&error, Error::BudgetExceeded(b) if *b == expected));
    assert!(limited.cancelled, "the later drain observed cancellation");
    assert_eq!(limited.exceeded, None);
    assert_eq!(limited.written(), prior as u64);
    assert_eq!(limited.get_ref().len(), prior);
    assert_eq!(sent.get(), 0);
    exact_refusal(Err(limited.classify(error)), expected);
}

#[test]
fn classifier_still_translates_its_actual_io_refusals() {
    let mut limited = LimitedWriter::new(Vec::new(), Some(0), None);
    let sent = RowCount::new();
    let error = write_buffered(&mut limited, &sent, None, &mut |out, _| {
        out.write_all(b"prefix").map_err(io)
    })
    .unwrap_err();
    assert!(matches!(&error, Error::Io(_)));
    assert!(
        matches!(limited.classify(error), Error::BudgetExceeded(b) if b.kind == BudgetKind::ResultBytes && b.limit == 0 && b.requested == 6)
    );
    assert_eq!((limited.written(), sent.get()), (0, 0));
    let cancel = Arc::new(AtomicBool::new(true));
    let mut limited = LimitedWriter::new(Vec::new(), None, Some(cancel));
    let error = write_buffered(&mut limited, &sent, None, &mut |out, _| {
        out.write_all(&vec![b'x'; CANCEL_CHECK_BYTES as usize])
            .map_err(io)
    })
    .unwrap_err();
    assert!(matches!(&error, Error::Io(_)));
    assert!(matches!(limited.classify(error), Error::Cancelled));
    assert_eq!((limited.written(), sent.get()), (0, 0));
}

#[test]
fn an_actual_parent_ownership_refusal_keeps_its_typed_source_through_serde() {
    use crate::sparql::ctx::Ctx;
    use sparkles_budget::{MemoryDomain, ParentRefused};
    for format in [Format::Native, Format::Tsv] {
        let store = Store::in_memory(StoreOptions::default());
        // Install the real parent before planning/allocation. Construction is
        // admitted, then only new writer payload admission is refused; no Ctx or
        // domain is swapped beneath already credited result storage.
        let refuse_positive = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let parent_gate = refuse_positive.clone();
        let parent = MemoryDomain::unpooled().parent(
            move |bytes| {
                if bytes == 0 || !parent_gate.load(std::sync::atomic::Ordering::Relaxed) {
                    Ok(())
                } else {
                    Err(ParentRefused::Overflow)
                }
            },
            |_| {},
        );
        let parsed = crate::sparql::parse_query("SELECT (1 AS ?v) WHERE {}", None, &[]).unwrap();
        let (pattern, _, _) = crate::sparql::split(&parsed);
        let mut context = Ctx::new(store.snapshot());
        context.set_memory_parent(parent).unwrap();
        let ctx = Arc::new(context);
        let mut planner = crate::sparql::plan::Planner::new(&ctx);
        planner.source = Some(&parsed);
        let node = planner
            .plan(pattern, &crate::sparql::ActiveGraph::Default, Vec::new())
            .unwrap();
        let plan_depth = node.depth();
        ctx.attach_owner();
        let prepared = crate::sparql::PreparedQuery {
            ctx: ctx.clone(),
            node,
            pattern: pattern.clone(),
            bound: Vec::new(),
            kind: QueryKind::Select,
            plan_depth,
            _rewrite_guard: None,
        };
        let r = crate::sparql::execute_prepared(&parsed, &Default::default(), 0.0, 0.0, prepared)
            .unwrap();
        assert_eq!(
            r.test_application_rows()[0][0],
            Some(Term::Literal(oxrdf::Literal::from(1)))
        );
        // Discard unused peak allowance while retaining every actual payload
        // credit, so the next positive writer request must reach this parent.
        ctx.shrink_retained(0).unwrap();
        let baseline = ctx.live_bytes();
        assert!(
            baseline > 0,
            "the result retains its own actual metadata/table credit"
        );
        refuse_positive.store(true, std::sync::atomic::Ordering::Relaxed);
        let sent = RowCount::new();
        let error = format.write(&r, Vec::new(), &sent).unwrap_err();
        let Error::Component(component) = error else {
            panic!("lost parent origin: {error:?}")
        };
        assert_eq!(
            (component.component, component.code.as_str()),
            ("query-memory", "ownership")
        );
        assert_eq!(
            component
                .source
                .as_ref()
                .unwrap()
                .downcast_ref::<ParentRefused>(),
            Some(&ParentRefused::Overflow)
        );
        assert_eq!(sent.get(), 0);
        assert_eq!(r.test_context().live_bytes(), baseline);
        drop(r);
        assert_eq!(ctx.live_bytes(), 0);
    }
}

/// A stored answer whose first value is a long vocabulary literal and whose second
/// is short, both served from the vocabulary rather than the query's local terms.
fn stored(long: usize) -> (tempfile::TempDir, QueryResult) {
    use crate::io::{RdfFormat, Source};
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path(), StoreOptions::default()).unwrap();
    let data = format!(
        "<urn:0> <urn:p> \"{}\" .\n<urn:1> <urn:p> \"a\" .\n",
        "z".repeat(long)
    );
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    let r = query(
        store.snapshot(),
        "SELECT ?o WHERE { ?s <urn:p> ?o } ORDER BY ?s",
        &QueryOptions {
            no_cache: true,
            max_memory_bytes: Some(MB),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.len(), 2);
    assert!(
        r.test_table().cols[0]
            .iter()
            .all(|id| id.tag() == crate::id::Tag::Vocab)
    );
    (directory, r)
}

#[test]
fn vocabulary_reconstruction_scratch_is_admitted_before_it_is_allocated() {
    const K: usize = 64 << 10;
    let (_directory, r) = stored(K);
    let before = r.test_context().live_bytes();
    // The row and its owned literal fit, but not the key's rebuild scratch beside it.
    let blocker = r
        .test_context()
        .charge(r.test_context().memory_remaining() - (K as u64 + 4096))
        .unwrap();
    let blocked = r.test_context().live_bytes();
    for format in [Format::Native, Format::Tsv] {
        let sent = RowCount::new();
        let error = format.write(&r, Vec::new(), &sent).unwrap_err();
        assert!(
            matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory && b.limit == MB),
            "{error:?}"
        );
        assert_eq!(sent.get(), 0);
        assert_eq!(r.test_context().live_bytes(), blocked);
        r.test_context().check_memory_failure().unwrap();
    }
    drop(blocker);
    for format in [Format::Native, Format::Tsv] {
        let sent = RowCount::new();
        format.write(&r, Vec::new(), &sent).unwrap();
        assert_eq!(sent.get(), 2);
        assert_eq!(r.test_context().live_bytes(), before);
    }
}

#[test]
fn a_recorded_refusal_stays_the_error_of_a_reused_row() {
    let r = answer(false);
    let mut row = output::ScratchRow::new(&r).unwrap();
    // Leave too little room for the long first cell, so its own growth would refuse
    // with a different request if the recorded refusal were not checked first.
    let blocker = r
        .test_context()
        .charge(r.test_context().memory_remaining() - 64)
        .unwrap();
    let budget = refuse(&r, false);
    exact_refusal(row.load(0).map(|_| ()), budget);
    drop(row);
    drop(blocker);
}

#[test]
fn an_outlier_row_does_not_make_a_later_small_row_refuse() {
    let (_directory, r) = stored(8192);
    let expected = r.test_application_rows();
    let live = r.test_context().live_bytes();
    let mut row = output::ScratchRow::new(&r).unwrap();
    assert_eq!(row.load(0).unwrap(), &expected[0][..]);
    // The writer keeps the outlier's credit. The small row fits inside it, so no
    // further admission happens however little room the query has left.
    let pressure = r
        .test_context()
        .charge(r.test_context().memory_remaining())
        .unwrap();
    assert_eq!(row.load(1).unwrap(), &expected[1][..]);
    drop(row);
    drop(pressure);
    assert_eq!(r.test_context().live_bytes(), live);
    r.test_context().check_memory_failure().unwrap();
}

/// A writer that cancels its query once `after` bytes have reached it.
struct CancelAfter {
    bytes: Vec<u8>,
    cancel: Arc<AtomicBool>,
    after: usize,
    flushes: usize,
}

impl Write for CancelAfter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert!(!bytes.is_empty());
        self.bytes.extend_from_slice(bytes);
        if self.bytes.len() >= self.after {
            self.cancel.store(true, Ordering::Relaxed);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn cancelling_during_a_grown_native_row_counts_it_and_stops_the_trailer() {
    let store = Store::in_memory(StoreOptions::default());
    let literal = serde_json::to_string(&"\n".repeat(40_000)).unwrap();
    let text = format!("SELECT ?o WHERE {{ VALUES ?o {{ {literal} }} }}");
    let r = query(store.snapshot(), &text, &QueryOptions::default()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let sink = CancelAfter {
        bytes: Vec::new(),
        cancel: cancel.clone(),
        after: CANCEL_CHECK_BYTES as usize,
        flushes: 0,
    };
    let mut out = LimitedWriter::new(sink, None, Some(cancel));
    let sent = RowCount::new();
    let result = write(
        Source::Result(&r),
        WriteOptions::new(super::Format::Solutions(SolutionsFormat::Sparkles), &sent),
        &mut out,
    );
    assert_eq!(sent.get(), 1, "the sink took the whole row");
    let result = result.map_err(|error| out.classify(error));
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert!(out.get_ref().bytes.ends_with(b"}]"));
}

#[test]
fn cancelling_during_the_last_grown_csv_or_tsv_row_fails_on_completion() {
    use crate::sparql::{CursorOptions, FallbackPolicy, select_cursor};
    use std::io::BufWriter;

    fn check<W: Write>(
        source: Source<'_>,
        format: SolutionsFormat,
        mut writer: W,
        limited: impl Fn(&mut W) -> &mut LimitedWriter<CancelAfter>,
        flushes: usize,
    ) {
        let sent = RowCount::new();
        let result = write_with_completion(
            source,
            WriteOptions::new(super::Format::Solutions(format), &sent),
            &mut writer,
            |out| {
                assert_eq!(sent.get(), 2, "completion follows the final row credit");
                limited(out).check_completion()
            },
        );
        assert!(
            matches!(result, Err(Error::Cancelled)),
            "{format:?}: {result:?}"
        );
        let out = limited(&mut writer);
        assert!(out.get_ref().cancel.load(Ordering::Relaxed));
        assert_eq!(out.get_ref().flushes, flushes);
        assert_eq!(out.written() as usize, out.get_ref().bytes.len());
        assert!(out.written() >= 70 << 10);
    }

    let store = Store::in_memory(StoreOptions::default());
    let literal = "x".repeat(70 << 10);
    let text = format!("SELECT ?o WHERE {{ VALUES ?o {{ \"first\" \"{literal}\" }} }}");
    let options = QueryOptions::default();
    let r = query(store.snapshot(), &text, &options).unwrap();
    for format in [SolutionsFormat::Csv, SolutionsFormat::Tsv] {
        for buffered in [false, true] {
            let mut cursor = select_cursor(
                store.snapshot(),
                &text,
                &options,
                &CursorOptions {
                    batch_rows: 1,
                    fallback: FallbackPolicy::RejectMaterialization,
                    ..Default::default()
                },
            )
            .unwrap();
            for (source, flushes) in [(Source::Result(&r), 0), (Source::Select(&mut cursor), 1)] {
                // Keep the sink's flag independent of the query and cursor controls.
                let cancel = Arc::new(AtomicBool::new(false));
                let sink = CancelAfter {
                    bytes: Vec::new(),
                    cancel: cancel.clone(),
                    after: CANCEL_CHECK_BYTES as usize,
                    flushes: 0,
                };
                let out = LimitedWriter::new(sink, None, Some(cancel));
                if buffered {
                    check(
                        source,
                        format,
                        BufWriter::with_capacity(OUTPUT_BUFFER, out),
                        BufWriter::get_mut,
                        flushes,
                    );
                } else {
                    check(source, format, out, |w| w, flushes);
                }
            }
            let stats = cursor.stats();
            assert_eq!(stats.status, crate::sparql::CursorStatus::Complete);
            assert_eq!(stats.error, Some(Error::Cancelled.to_string()));
        }
    }
}

#[test]
fn a_small_buffered_result_checks_the_reused_adapters_checkpoint() {
    use std::io::BufWriter;

    let cancel = Arc::new(AtomicBool::new(false));
    let mut limited = LimitedWriter::new(Vec::new(), None, Some(cancel.clone()));
    limited
        .write_all(&vec![b'a'; CANCEL_CHECK_BYTES as usize - 1])
        .unwrap();
    cancel.store(true, Ordering::Relaxed);
    limited.check_completion().unwrap();
    cancel.store(false, Ordering::Relaxed);
    limited
        .write_all(&vec![b'a'; CANCEL_CHECK_BYTES as usize])
        .unwrap();
    let written = limited.written();
    let next_check = limited.next_check;
    // A successful completion check leaves the pending checkpoint in place.
    limited.check_completion().unwrap();
    cancel.store(true, Ordering::Relaxed);

    let mut out = BufWriter::new(limited);
    let store = Store::in_memory(StoreOptions::default());
    let r = query(
        store.snapshot(),
        "SELECT ?o WHERE { VALUES ?o { \"new\" } }",
        &QueryOptions::default(),
    )
    .unwrap();
    let sent = RowCount::new();
    let result = write_with_completion(
        Source::Result(&r),
        WriteOptions::new(super::Format::Solutions(SolutionsFormat::Csv), &sent),
        &mut out,
        |w| w.get_mut().check_completion(),
    );
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert_eq!(sent.get(), 1);
    assert_eq!(out.buffer(), b"o\r\nnew\r\n");
    assert_eq!(out.get_ref().written(), written);
    assert_eq!(out.get_ref().next_check, next_check);
    assert!(out.get_ref().cancelled);

    let mut unlimited = LimitedWriter::new(Vec::new(), None, None);
    unlimited
        .write_all(&vec![b'a'; CANCEL_CHECK_BYTES as usize])
        .unwrap();
    unlimited.check_completion().unwrap();
}

#[test]
fn plain_writers_receive_only_result_bytes_and_their_existing_flushes() {
    use std::io::{self, LineWriter};

    #[derive(Default)]
    struct Sink {
        short_line: bool,
        newline_retries: usize,
        empty_pieces: usize,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            match bytes {
                b"tail\r\n" if self.short_line => Ok(bytes.len() - 1),
                b"\n" if self.short_line => {
                    self.newline_retries += 1;
                    Err(io::ErrorKind::BrokenPipe.into())
                }
                _ => Ok(bytes.len()),
            }
        }
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            if bytes.is_empty() {
                self.empty_pieces += 1;
                return Err(io::Error::other("unexpected empty piece"));
            }
            Ok(())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let store = Store::in_memory(StoreOptions::default());
    let literal = "x".repeat(70 << 10);
    let r = query(
        store.snapshot(),
        &format!("SELECT ?o WHERE {{ VALUES ?o {{ \"{literal}\" \"tail\" }} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let sent = RowCount::new();
    let options = || WriteOptions::new(super::Format::Solutions(SolutionsFormat::Csv), &sent);
    let mut out = LineWriter::new(Sink {
        short_line: true,
        ..Sink::default()
    });
    write(Source::Result(&r), options(), &mut out).unwrap();
    assert_eq!(sent.get(), 2);
    assert_eq!(out.get_ref().newline_retries, 0);
    assert_eq!(out.flush().unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(out.get_ref().newline_retries, 1);

    let mut out = Sink::default();
    let sent = RowCount::new();
    write(
        Source::Result(&r),
        WriteOptions::new(super::Format::Solutions(SolutionsFormat::Csv), &sent),
        &mut out,
    )
    .unwrap();
    assert_eq!(out.empty_pieces, 0);
    assert_eq!(sent.get(), 2);
}

#[test]
fn a_partial_row_sink_failure_bypasses_completion() {
    use std::io;

    struct Sink {
        cancel: Arc<AtomicBool>,
        left: usize,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.left == 0 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let n = bytes.len().min(self.left);
            self.left -= n;
            if self.left == 0 {
                self.cancel.store(true, Ordering::Relaxed);
            }
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let store = Store::in_memory(StoreOptions::default());
    let literal = "x".repeat(70 << 10);
    let r = query(
        store.snapshot(),
        &format!("SELECT ?o WHERE {{ VALUES ?o {{ \"first\" \"{literal}\" }} }}"),
        &QueryOptions::default(),
    )
    .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let sink = Sink {
        cancel: cancel.clone(),
        left: 1024,
    };
    let mut out = LimitedWriter::new(sink, None, Some(cancel.clone()));
    let sent = RowCount::new();
    let error = write_with_completion(
        Source::Result(&r),
        WriteOptions::new(super::Format::Solutions(SolutionsFormat::Csv), &sent),
        &mut out,
        |_| panic!("the sink failed before completion"),
    )
    .unwrap_err();
    assert!(matches!(out.classify(error), Error::Io(e) if e.kind() == io::ErrorKind::BrokenPipe));
    assert!(cancel.load(Ordering::Relaxed));
    assert_eq!(out.written(), 1024);
    assert_eq!(sent.get(), 1);
}

#[test]
fn a_flush_after_a_partial_long_write_preserves_io_errors() {
    use std::io;

    struct ShortSink {
        cancel: Arc<AtomicBool>,
        flush_error: Option<io::ErrorKind>,
    }

    impl Write for ShortSink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() as u64 >= CANCEL_CHECK_BYTES {
                self.cancel.store(true, Ordering::Relaxed);
            }
            Ok(bytes.len().min(1))
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_error.map_or(Ok(()), |kind| Err(kind.into()))
        }
    }

    let bytes = vec![b'x'; CANCEL_CHECK_BYTES as usize];
    for flush_error in [None, Some(io::ErrorKind::BrokenPipe)] {
        let cancel = Arc::new(AtomicBool::new(false));
        let sink = ShortSink {
            cancel: cancel.clone(),
            flush_error,
        };
        let mut out = LimitedWriter::new(sink, None, Some(cancel));
        out.write_all(&bytes[..bytes.len() - 1]).unwrap();
        // A small partial write crosses the poll boundary before the long piece.
        assert_eq!(out.write(b"xx").unwrap(), 1);
        assert_eq!(out.write(&bytes).unwrap(), 1);
        assert_eq!(out.written(), CANCEL_CHECK_BYTES + 1);
        let error = out.flush().unwrap_err();
        let error = out.classify(Error::Io(error));
        match flush_error {
            None => assert!(matches!(error, Error::Cancelled), "{error:?}"),
            Some(kind) => assert!(matches!(error, Error::Io(e) if e.kind() == kind)),
        }
    }
}

#[test]
fn an_eager_thrift_answer_checks_its_controls_before_each_row() {
    let r = answer(false);
    // The first row's literal is longer than the output buffer, so it reaches the
    // writer before the second row is decoded, and the writer cancels the query then.
    let mut out = CancelAfter {
        bytes: Vec::new(),
        cancel: r.test_context().cancel.clone(),
        after: LONG,
        flushes: 0,
    };
    let sent = RowCount::new();
    let error = write(
        Source::Result(&r),
        WriteOptions::new(super::Format::ResultsThrift, &sent),
        &mut out,
    )
    .unwrap_err();
    assert!(matches!(error, Error::Cancelled), "{error:?}");
    assert_eq!(sent.get(), 1);
    let text = String::from_utf8_lossy(&out.bytes);
    assert!(text.contains(&"x".repeat(LONG)));
    assert!(
        !text.contains("tail"),
        "the second row was written after the cancel"
    );
    r.test_context().cancel.store(false, Ordering::Relaxed);
}

#[test]
fn an_eager_thrift_answer_takes_no_vocabulary_prefetch() {
    use crate::algo::memory::probe;
    if !crate::index::io_hints() {
        return;
    }
    // Each arm has a fresh store, since a prefetch marks its vocabulary blocks as asked.
    let arm = |format: super::Format| {
        let (_directory, r) = stored(64);
        let _scope = probe::install(|_| {}, Some("answer-prefetch-ids"));
        let sent = RowCount::new();
        let result = write(
            Source::Result(&r),
            WriteOptions::new(format, &sent),
            &mut Vec::new(),
        );
        (result.map(drop), sent.get())
    };
    // The TSV writer prefetches the answer's vocabulary blocks, so the injected
    // failure shows that the probe is reached at all.
    let (tsv, _) = arm(super::Format::Solutions(SolutionsFormat::Tsv));
    assert!(
        matches!(&tsv, Err(Error::Invalid(m)) if m.contains("injected chunk storage")),
        "{tsv:?}"
    );
    let (thrift, rows) = arm(super::Format::ResultsThrift);
    thrift.unwrap();
    assert_eq!(rows, 2);
}

#[test]
fn an_eager_thrift_answer_checks_its_controls_after_the_last_rows_decode() {
    use crate::algo::memory::probe;
    let r = answer(false);
    // cancel while the last row is decoded, after its pre-row check
    let cancel = r.test_context().cancel.clone();
    let _scope = probe::install(
        move |point| {
            if let probe::Point::Allocation("scratch-row-decoded", 1) = point {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        None,
    );
    let sent = RowCount::new();
    let mut out = Vec::new();
    let error = write(
        Source::Result(&r),
        WriteOptions::new(super::Format::ResultsThrift, &sent),
        &mut out,
    )
    .unwrap_err();
    assert!(matches!(error, Error::Cancelled), "{error:?}");
    // the first row was whole before the second was decoded
    assert_eq!(sent.get(), 1);
    assert!(!String::from_utf8_lossy(&out).contains("tail"));
    r.test_context().cancel.store(false, Ordering::Relaxed);
}
