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
    fn write(self, r: &QueryResult, out: impl Write, sent: &Cell<u64>) -> Result<()> {
        match self {
            Self::Native => write_native_json_counted(r, out, None, None, sent),
            Self::Tsv => write_solutions_counted(r, SolutionsFormat::Tsv, out, None, sent),
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
    assert_eq!((r.len(), r.table.width()), (2, 1));
    // Establish that both actual local terms fit, rather than mistake setup refusal
    // or a genuine UNDEF for the writer defect.
    let values = r.rows();
    assert_eq!(values.len(), 2);
    assert!(matches!(&values[0][0], Some(Term::Literal(v)) if v.value() == "x".repeat(LONG)));
    assert!(matches!(&values[1][0], Some(Term::Literal(v)) if v.value() == "tail"));
    r.ctx.check_memory_failure().unwrap();
    r
}

fn refuse(r: &QueryResult, shared: bool) -> Budget {
    let bytes = if shared { 16 * MB } else { 2 * MB };
    let error = match r.ctx.charge(bytes) {
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
    r.ctx.fail_memory(&Error::BudgetExceeded(budget));
    assert!(matches!(r.ctx.check_memory_failure(), Err(Error::BudgetExceeded(b)) if b == budget));
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
        let sent = Cell::new(0);
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
        sent.set(0);
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
    let sent = Cell::new(0);
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
    let sent = Cell::new(0);
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
        let sent = Cell::new(0);
        assert!(
            matches!(format.write(&r, FailIo, &sent), Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe)
        );
        assert_eq!(sent.get(), 0);
        r.ctx.check_memory_failure().unwrap();
    }
}

#[test]
fn a_private_row_keeps_real_credit_until_its_payload_is_dropped() {
    for shared in [false, true] {
        let r = answer(shared);
        let before = r.ctx.live_bytes();
        let row = output::ScratchRow::decode(&r, 0).unwrap();
        assert!(matches!(&row.terms[0], Some(Term::Literal(v)) if v.value().len() == LONG));
        assert!(r.ctx.live_bytes() >= before + LONG as u64);
        drop(row);
        assert_eq!(r.ctx.live_bytes(), before);
        r.ctx.check_memory_failure().unwrap();
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
    assert_eq!((r.len(), r.table.width()), (1, 2));
    let before = r.ctx.live_bytes();
    let left = LONG as u64 + 512;
    let blocker = r
        .ctx
        .charge(MB.checked_sub(before + left).unwrap())
        .unwrap();
    let blocked = r.ctx.live_bytes();
    assert_eq!(blocked, MB - left);
    // A single actual decoded cell fits, including its owned string. The two
    // independent cells together exceed this same fixed remaining allowance.
    let cell_credit = r.ctx.charge(0).unwrap();
    let cell = r.ctx.output_term(r.table.cols[0][0], &cell_credit).unwrap();
    assert!(matches!(&cell, Some(Term::Literal(v)) if v.value().len() == LONG));
    drop(cell);
    drop(cell_credit);
    assert_eq!(r.ctx.live_bytes(), blocked);
    for format in [Format::Native, Format::Tsv] {
        let mut out = Vec::new();
        let sent = Cell::new(0);
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
            r.ctx.live_bytes(),
            blocked,
            "failed row returned only its own credit"
        );
        r.ctx.check_memory_failure().unwrap();
    }
    drop(blocker);
    assert_eq!(r.ctx.live_bytes(), before);
    for format in [Format::Native, Format::Tsv] {
        let sent = Cell::new(0);
        format.write(&r, Vec::new(), &sent).unwrap();
        assert_eq!(sent.get(), 1);
        assert_eq!(r.ctx.live_bytes(), before);
    }
}

fn classifier_late_cap(shared: bool) {
    let r = answer(shared);
    let expected = Cell::new(None);
    let mut limited = LimitedWriter::new(Vec::new(), Some(0), None);
    let sent = Cell::new(0);
    let error = write_buffered(&mut limited, &sent, &mut |out, _| {
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
    let sent = Cell::new(0);
    let error = write_buffered(&mut limited, &sent, &mut |out, _| {
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
    let sent = Cell::new(0);
    let error = write_buffered(&mut limited, &sent, &mut |out, _| {
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
    let error = write_buffered(&mut limited, &sent, &mut |out, _| {
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
        let mut r = query(
            store.snapshot(),
            "SELECT (1 AS ?v) WHERE {}",
            &Default::default(),
        )
        .unwrap();
        assert_eq!(r.rows()[0][0], Some(Term::Literal(oxrdf::Literal::from(1))));
        // The real nested-parent adapter admits zero credit but refuses required
        // positive payload admission with a structured parent ownership error.
        let parent = MemoryDomain::unpooled().parent(
            |bytes| {
                if bytes == 0 {
                    Ok(())
                } else {
                    Err(ParentRefused::Overflow)
                }
            },
            |_| {},
        );
        let mut ctx = Ctx::new(store.snapshot());
        ctx.set_memory_parent(parent).unwrap();
        r.ctx = Arc::new(ctx);
        let sent = Cell::new(0);
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
        assert_eq!(r.ctx.live_bytes(), 0);
    }
}
