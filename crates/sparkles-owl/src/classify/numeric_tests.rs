use super::super::data::{DataVal, Known, Value};
use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

/// Thread-local requested-storage measurement. Inputs/guards and observer buffers are
/// prepared before enabling it and remain alive through the measured insertion region.
/// Realloc peaks conservatively include old/new overlap. No hook allocates.
#[derive(Clone, Copy, Default, Debug)]
pub(in crate::classify) struct Sample {
    enabled: bool,
    pub live: usize,
    pub peak: usize,
    pub events: usize,
    pub alloc_events: usize,
    pub realloc_events: usize,
    pub dealloc_events: usize,
    pub requested_alloc_bytes: usize,
    pub requested_realloc_bytes: usize,
    pub replaced_realloc_bytes: usize,
}
thread_local! {
    static SAMPLE: Cell<Sample> = const { Cell::new(Sample {
        enabled: false, live: 0, peak: 0, events: 0,
        alloc_events: 0, realloc_events: 0, dealloc_events: 0,
        requested_alloc_bytes: 0, requested_realloc_bytes: 0, replaced_realloc_bytes: 0,
    }) };
}
struct Counting;
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn allocated(bytes: usize) {
    let _ = SAMPLE.try_with(|s| {
        let mut v = s.get();
        if v.enabled {
            v.live += bytes;
            v.peak = v.peak.max(v.live);
            v.events += 1;
            v.alloc_events += 1;
            v.requested_alloc_bytes += bytes;
            s.set(v);
        }
    });
}
fn freed(bytes: usize) {
    let _ = SAMPLE.try_with(|s| {
        let mut v = s.get();
        if v.enabled {
            v.live = v.live.saturating_sub(bytes);
            v.dealloc_events += 1;
            s.set(v);
        }
    });
}
fn reallocated(old: usize, new: usize) {
    let _ = SAMPLE.try_with(|s| {
        let mut v = s.get();
        if v.enabled {
            v.live += new;
            v.peak = v.peak.max(v.live);
            v.live = v.live.saturating_sub(old);
            v.events += 1;
            v.realloc_events += 1;
            v.requested_realloc_bytes += new;
            v.replaced_realloc_bytes += old;
            s.set(v);
        }
    });
}
// SAFETY: pointers, layouts and sizes are forwarded unchanged to System. All
// instrumentation is a constant-initialized thread-local Cell and never allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            allocated(layout.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            allocated(layout.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        freed(layout.size());
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let out = unsafe { System.realloc(p, layout, size) };
        if !out.is_null() {
            reallocated(layout.size(), size);
        }
        out
    }
}
pub(super) fn sample() -> Sample {
    SAMPLE.with(Cell::get)
}
pub(in crate::classify) fn measure<T>(f: impl FnOnce() -> T) -> (T, Sample) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SAMPLE.with(|s| {
                let mut v = s.get();
                v.enabled = false;
                s.set(v);
            });
        }
    }
    SAMPLE.with(|s| {
        assert!(!s.get().enabled);
        s.set(Sample {
            enabled: true,
            ..Sample::default()
        });
    });
    let reset = Reset;
    let out = f();
    let stats = sample();
    drop(reset);
    (out, stats)
}
fn budget() -> Arc<Budget> {
    Budget::new(&Limits::default(), 0, None)
}
fn number(s: &str, k: Kind) -> Number {
    parse(s, k, &budget()).unwrap().unwrap()
}
const N: &str = "170141183460469231731687303715884105728";
const TWICE: &str = "340282366920938463463374607431768211456";

#[test]
fn exact_identity_hash_order_and_small_canonicalization() {
    use std::hash::BuildHasher;
    let routes = [
        number(N, Kind::Integer),
        number(&format!("{N}.000"), Kind::Decimal),
        number(&format!("{TWICE}/2"), Kind::Rational),
        number(&format!("+000{N}"), Kind::NonNegativeInteger),
    ];
    for a in &routes {
        for b in &routes {
            assert_eq!(a, b);
            assert_eq!(a.cmp(b), Ordering::Equal);
            assert_eq!(
                rustc_hash::FxBuildHasher.hash_one(a),
                rustc_hash::FxBuildHasher.hash_one(b)
            );
        }
    }
    assert!(matches!(routes[0], Number::Big(_)));
    let one = number(&format!("{N}/{N}"), Kind::Rational);
    assert!(matches!(one, Number::Small { n: 1, d: 1, .. }));
    assert_eq!(
        number(&format!("0/{N}"), Kind::Rational),
        number("-0", Kind::Integer)
    );
    assert_eq!(
        number(&format!("-{N}"), Kind::Integer),
        number(&format!("-{TWICE}/2"), Kind::Rational)
    );
    assert!(matches!(
        number(&format!("-{N}"), Kind::Integer),
        Number::Small { n: i128::MIN, .. }
    ));
    assert!(number(&format!("-{N}"), Kind::Integer) < number("-1", Kind::Integer));
    assert!(number("-1", Kind::Integer) < number(N, Kind::Integer));
    assert!(
        number(N, Kind::Integer) < number("170141183460469231731687303715884105729", Kind::Integer)
    );
    assert_eq!(number(".5", Kind::Decimal), number("1/2", Kind::Rational));
}

#[test]
fn lexical_scanner_agrees_with_the_unchanged_profile_validator() {
    let adversarial = [
        "".to_string(),
        "+".into(),
        "-".into(),
        "0".into(),
        "-0".into(),
        "-1".into(),
        "+0001".into(),
        ".0".into(),
        "1.".into(),
        ".".into(),
        "+.5".into(),
        "1e2".into(),
        "1/2".into(),
        "1/+2".into(),
        "1/-2".into(),
        "1/0".into(),
        "0/000".into(),
        "1/0002".into(),
        "1//2".into(),
        "1.2.3".into(),
        "１".into(),
        " 1".into(),
        "1\n".into(),
        format!("{}1", "0".repeat(4096)),
        format!("{}x", "0".repeat(4096)),
        format!("0.{}1", "0".repeat(4096)),
    ];
    for (kind, dt) in [
        (Kind::Integer, crate::vocab::XSD.to_owned() + "integer"),
        (
            Kind::NonNegativeInteger,
            crate::vocab::XSD.to_owned() + "nonNegativeInteger",
        ),
        (Kind::Decimal, crate::vocab::XSD.to_owned() + "decimal"),
        (Kind::Rational, crate::vocab::OWL_RATIONAL.to_owned()),
    ] {
        for s in &adversarial {
            assert_eq!(
                scan(s, kind, &budget()).unwrap().is_some(),
                crate::datatypes::lexical(&dt, s) == Some(true),
                "{dt} {s:?}"
            );
        }
    }
}

#[test]
fn cached_membership_has_no_allocator_factor_or_guard_work() {
    // 10^40 has only 2/5 factors; 3*10^40 retains a factor 3 after reduction.
    let b = budget();
    let decimal = parse(
        "1/10000000000000000000000000000000000000000",
        Kind::Rational,
        &b,
    )
    .unwrap()
    .unwrap();
    let third = parse(
        "1/30000000000000000000000000000000000000000",
        Kind::Rational,
        &b,
    )
    .unwrap()
    .unwrap();
    let integer = parse(N, Kind::Integer, &b).unwrap().unwrap();
    let polls = b.polls.load(AtomicOrdering::Relaxed);
    let (_, stats) = measure(|| {
        for _ in 0..256 {
            assert!(decimal.is_a(Kind::Decimal));
            assert!(!third.is_a(Kind::Decimal));
            assert!(integer.is_a(Kind::NonNegativeInteger));
            assert!(!decimal.is_a(Kind::Integer));
            let known = DataVal {
                known: Known::Value(Value::Num(decimal.clone())),
                opaque: Vec::new(),
            };
            let kind = DataVal {
                known: Known::Kind(Kind::Decimal),
                opaque: Vec::new(),
            };
            assert!(known.within(&kind));
            assert_eq!(known.meet(&kind), known);
        }
    });
    assert_eq!(stats.events, 0);
    assert_eq!(b.polls.load(AtomicOrdering::Relaxed), polls);
    // Metadata must not participate in identity or hashing.
    if let Number::Big(big) = integer {
        let mut corrupt = Number::Big(big.clone());
        drop(big);
        let Number::Big(v) = &mut corrupt else {
            unreachable!()
        };
        // A fresh independent parse gives a different ledger but the same identity.
        let equal = number(N, Kind::Integer);
        Arc::get_mut(v).unwrap().membership = Membership {
            integer: false,
            decimal: false,
            nonnegative: false,
        };
        assert_eq!(corrupt, equal);
        use std::hash::BuildHasher;
        assert_eq!(
            rustc_hash::FxBuildHasher.hash_one(&corrupt),
            rustc_hash::FxBuildHasher.hash_one(&equal)
        );
    }
}

#[test]
fn a_last_alias_retains_the_charge_and_clone_never_copies_limbs() {
    let b = budget();
    let n = parse(N, Kind::Integer, &b).unwrap().unwrap();
    let ledger = b.ledger().unwrap();
    let charged = ledger.live.load(AtomicOrdering::Relaxed);
    assert!(charged > 0);
    let (aliases, stats) = measure(|| std::array::from_fn::<_, 16, _>(|_| n.clone()));
    assert_eq!(stats.events, 0);
    assert_eq!(ledger.live.load(AtomicOrdering::Relaxed), charged);
    drop(n);
    drop(b);
    assert_eq!(ledger.live.load(AtomicOrdering::Relaxed), charged);
    drop(aliases);
    assert_eq!(ledger.live.load(AtomicOrdering::Relaxed), 0);
}

#[test]
fn refusing_scratch_precedes_the_first_numeric_allocation() {
    let b = Budget::new(
        &Limits {
            charge: Some(Arc::new(|_| false)),
            ..Limits::default()
        },
        0,
        None,
    );
    let (r, stats) = measure(|| parse(N, Kind::Integer, &b));
    assert!(matches!(r, Err(Error::Memory { .. })));
    assert_eq!(stats.events, 0);
    assert_eq!(b.held.lock().scratch, 0);
    assert!(b.ledger().is_none());
}

#[test]
fn refusing_publication_precedes_output_allocation_and_releases_scratch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let before = Arc::new(AtomicUsize::new(usize::MAX));
    let refused = Arc::new(AtomicUsize::new(usize::MAX));
    let (c, r) = (calls.clone(), refused.clone());
    let b = Budget::new(
        &Limits {
            charge: Some(Arc::new(move |_| {
                if c.fetch_add(1, AtomicOrdering::Relaxed) == 0 {
                    true
                } else {
                    r.store(sample().events, AtomicOrdering::Relaxed);
                    false
                }
            })),
            ..Limits::default()
        },
        0,
        None,
    );
    let p = before.clone();
    b.set_hook(Arc::new(move |point| {
        if point == Point::Publish {
            p.store(sample().events, AtomicOrdering::Relaxed);
        }
        Ok(())
    }));
    let (r, stats) = measure(|| parse(N, Kind::Integer, &b));
    assert!(matches!(r, Err(Error::Memory { .. })));
    assert_eq!(
        before.load(AtomicOrdering::Relaxed),
        refused.load(AtomicOrdering::Relaxed)
    );
    assert_eq!(stats.events, refused.load(AtomicOrdering::Relaxed));
    assert_eq!(b.held.lock().scratch, 0);
    assert!(b.ledger().is_none());
}

#[test]
fn stops_occur_in_actual_numeric_and_adversarial_lexical_work() {
    let cases = [
        (
            Point::Lexical,
            format!("{}1", "0".repeat(4096)),
            Kind::Integer,
        ),
        (
            Point::LexicalFinal,
            format!("{}x", "0".repeat(4096)),
            Kind::Integer,
        ),
        (Point::Digit, N.to_string(), Kind::Integer),
        (Point::Gcd, format!("{TWICE}/3"), Kind::Rational),
        (
            Point::Factor,
            "1/10000000000000000000000000000000000000000".to_string(),
            Kind::Rational,
        ),
    ];
    for (point, input, kind) in cases {
        for why in [Error::Cancelled, Error::Timeout] {
            let b = budget();
            let e = why.clone();
            let hit = Arc::new(AtomicUsize::new(0));
            let h = hit.clone();
            b.set_hook(Arc::new(move |p| {
                if p == point {
                    h.fetch_add(1, AtomicOrdering::Relaxed);
                    Err(e.clone())
                } else {
                    Ok(())
                }
            }));
            assert_eq!(parse(&input, kind, &b).unwrap_err(), why, "{point:?}");
            assert_eq!(hit.load(AtomicOrdering::Relaxed), 1);
            assert_eq!(b.held.lock().scratch, 0);
            assert!(
                b.ledger()
                    .is_none_or(|l| l.live.load(AtomicOrdering::Relaxed) == 0)
            );
            b.set_hook(Arc::new(|_| Ok(())));
            assert!(parse(N, Kind::Integer, &b).unwrap().is_some());
        }
    }
}

#[test]
fn workspace_covers_threshold_operands_and_output_overlap() {
    // On 64-bit native digits these straddle 32/64/128/256 digit branches. The
    // numerator is twice the denominator length, forcing the audited large division
    // entry shapes rather than testing only multiplication by scalar 10.
    for digits in [
        600usize, 640, 1200, 1240, 2450, 2470, 4900, 5000, 19600, 20000,
    ] {
        let make = |len: usize, seed: u64| {
            let mut x = seed;
            (0..len)
                .map(|_| {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                    char::from(b'1' + ((x >> 32) % 9) as u8)
                })
                .collect::<String>()
        };
        let n = make(digits, 17);
        let d = make(digits / 2, 43);
        let input = format!("{n}/{d}");
        let b = budget();
        let (value, stats) = measure(|| parse(&input, Kind::Rational, &b).unwrap().unwrap());
        let Number::Big(ref payload) = value else {
            panic!("threshold case unexpectedly small")
        };
        let allowance = workspace(digits, digits / 2).unwrap();
        assert!(
            stats.peak <= allowance,
            "{digits}: {stats:?}, W={allowance}"
        );
        assert!(stats.live <= b.ledger().unwrap().bytes());
        assert_eq!(b.held.lock().scratch, 0);
        assert!(b.peak() >= allowance + payload._token.bytes);
        drop(value);
        assert_eq!(b.ledger().unwrap().live.load(AtomicOrdering::Relaxed), 0);
    }
    assert!(workspace(usize::MAX, 1).is_err());
    assert!(workspace(1, usize::MAX).is_err());
}

#[test]
fn simultaneous_constructors_share_one_admission_and_reuse_peak_credit() {
    use std::sync::Barrier;
    let w = workspace(39, 1).unwrap();
    let b = Budget::new(
        &Limits {
            max_memory: Some(w + w / 2),
            ..Limits::default()
        },
        0,
        None,
    );
    let barrier = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (held, wait) = (barrier.clone(), release.clone());
    b.set_hook(Arc::new(move |point| {
        if point == Point::Digit {
            held.wait();
            wait.wait();
        }
        Ok(())
    }));
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| parse(N, Kind::Integer, &b));
        barrier.wait();
        assert!(matches!(
            parse(N, Kind::Integer, &b),
            Err(Error::Memory { .. })
        ));
        // Disable the per-digit latch before releasing the first operation.
        b.set_hook(Arc::new(|_| Ok(())));
        release.wait();
        assert!(worker.join().unwrap().unwrap().is_some());
    });
    assert_eq!(b.held.lock().scratch, 0);
    let granted = Arc::new(AtomicUsize::new(0));
    let g = granted.clone();
    let b = Budget::new(
        &Limits {
            charge: Some(Arc::new(move |bytes| {
                g.fetch_add(bytes, AtomicOrdering::Relaxed);
                true
            })),
            ..Limits::default()
        },
        0,
        None,
    );
    drop(parse(N, Kind::Integer, &b).unwrap());
    let first = granted.load(AtomicOrdering::Relaxed);
    drop(parse(N, Kind::Integer, &b).unwrap());
    assert_eq!(first, granted.load(AtomicOrdering::Relaxed));
}

#[test]
fn real_index_matching_reads_cached_membership_without_allocation() {
    let doc = crate::ofn_read::parse(include_str!(
        "../../tests/fixtures/precision663/decimal-membership.ofn"
    ))
    .unwrap();
    let refs: Vec<_> = doc.axioms.iter().map(|a| &a.kind).collect();
    let mut sat = crate::classify::Saturation::new();
    sat.classify(&refs, None, &Limits::default()).unwrap();
    let ix = sat.index.as_ref().unwrap();
    let role = sat.interner.roles.iter().position(|r| matches!(r, super::super::index::RoleName::Data(d) if d.as_str() == "urn:precision663:d")).unwrap() as u32;
    let value = ix.vals.iter().find(|v| v.single().is_some()).unwrap();
    let mut matches = 0;
    let (_, stats) = measure(|| {
        for _ in 0..256 {
            ix.data_matches(role, value, |_| matches += 1);
        }
    });
    assert!(matches > 0);
    assert_eq!(stats.events, 0);
}

#[test]
fn both_actual_index_copies_refuse_before_their_vector_allocation() {
    use super::super::index::{Axioms, Halt, Index, Interner, normalize};
    for stop_copy in [1, 2] {
        let doc = crate::ofn_read::parse(include_str!(
            "../../tests/fixtures/precision663/integer-decimal.ofn"
        ))
        .unwrap();
        let mut it = Interner::new();
        let mut ax = Axioms::default();
        for a in &doc.axioms {
            assert!(normalize(&a.kind, &mut it, &mut ax).is_none());
        }
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let c = cancelled.clone();
        let b = Budget::new(
            &Limits {
                cancelled: Some(Arc::new(move || c.load(AtomicOrdering::Relaxed))),
                ..Limits::default()
            },
            0,
            it.ranges.ledger.clone(),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let marker = Arc::new(AtomicUsize::new(usize::MAX));
        let (c, m, flag) = (calls.clone(), marker.clone(), cancelled.clone());
        b.set_hook(Arc::new(move |point| {
            if point == Point::IndexCopy && c.fetch_add(1, AtomicOrdering::Relaxed) + 1 == stop_copy
            {
                m.store(sample().events, AtomicOrdering::Relaxed);
                flag.store(true, AtomicOrdering::Relaxed);
            }
            Ok(())
        }));
        it.ranges.begin(b);
        let (result, stats) = measure(|| Index::build(&ax, &mut it, None, &|| false));
        assert!(matches!(result, Err(Halt::Stopped)));
        assert_eq!(calls.load(AtomicOrdering::Relaxed), stop_copy);
        assert_eq!(
            stats.events,
            marker.load(AtomicOrdering::Relaxed),
            "copy {stop_copy} allocated after its stop"
        );
        it.ranges.end();
        it.ranges.begin(budget());
        assert!(Index::build(&ax, &mut it, None, &|| false).is_ok());
    }
}

#[test]
fn shared_payload_does_not_skip_interner_handle_admission() {
    use super::super::index::Interner;
    let n = number(N, Kind::Integer);
    let value = DataVal {
        known: Known::Value(Value::Num(n)),
        opaque: Vec::new(),
    };
    let mut it = Interner::new();
    it.ranges.begin(Budget::new(
        &Limits {
            charge: Some(Arc::new(|_| false)),
            ..Limits::default()
        },
        0,
        None,
    ));
    let (_, stats) = measure(|| it.val(value));
    assert!(matches!(it.ranges.check(), Err(Error::Memory { .. })));
    assert_eq!(stats.events, 0);
    assert!(it.vals.is_empty());
}

#[test]
fn compiled_numeric_owner_sizes_are_recorded() {
    println!(
        "numeric owner sizes: Number={} Value={} DataVal={} CanonicalBigNumber={} NumericLedger={} Group={} Extra={}",
        size_of::<Number>(),
        size_of::<Value>(),
        size_of::<DataVal>(),
        size_of::<CanonicalBigNumber>(),
        size_of::<NumericLedger>(),
        size_of::<super::super::saturate::Group>(),
        size_of::<super::super::saturate::Extra>()
    );
}

#[test]
fn a_parallel_numeric_memory_stop_cannot_merge_an_axiom_away() {
    // Unit builds deliberately lower PARALLEL_AXIOMS to 64. The pool accepts ordinary
    // chunk/name growth and refuses only the much larger numeric workspace grant.
    let mut text = String::from(
        "Prefix(:=<urn:precision663:>) Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) Ontology(",
    );
    for i in 0..96 {
        text.push_str(&format!("Declaration(Class(:C{i})) "));
    }
    text.push_str("SubClassOf(:A DataHasValue(:d \"170141183460469231731687303715884105728\"^^xsd:integer)) SubClassOf(DataHasValue(:d \"170141183460469231731687303715884105728.0\"^^xsd:decimal) :B))");
    let doc = crate::ofn_read::parse(&text).unwrap();
    let refs: Vec<_> = doc.axioms.iter().map(|a| &a.kind).collect();
    let mut sat = crate::classify::Saturation::new();
    let limits = Limits {
        threads: 4,
        charge: Some(Arc::new(|bytes| bytes < 1_000_000)),
        ..Limits::default()
    };
    assert!(
        matches!(
            sat.classify(&refs, None, &limits),
            Err(Error::Memory { .. })
        ),
        "a refused chunk must retry serially and preserve the numeric error"
    );
    let out = sat
        .classify(
            &refs,
            None,
            &Limits {
                threads: 4,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(out.consistent && out.complete);
    assert!(sat.excluded().is_empty());
    assert!(out.taxonomy.is_subclass(
        &crate::model::Class::new("urn:precision663:A"),
        &crate::model::Class::new("urn:precision663:B")
    ));
}

#[test]
fn retained_transfer_preserves_aggregate_storage_and_reuses_peak_without_a_grant() {
    let granted = Arc::new(AtomicUsize::new(0));
    let g = granted.clone();
    let b = Budget::new(
        &Limits {
            charge: Some(Arc::new(move |n| {
                g.fetch_add(n, AtomicOrdering::Relaxed);
                true
            })),
            ..Limits::default()
        },
        64,
        None,
    );
    let first = b.reserve(512).unwrap();
    let second = b.reserve(128).unwrap();
    assert_eq!(b.retained_storage(), (64, 640));
    assert_eq!(b.peak(), 704);
    first.retain(Some(300)).unwrap();
    assert_eq!(b.retained_storage(), (364, 128));
    assert_eq!(granted.load(AtomicOrdering::Relaxed), 704);
    drop(second);
    assert_eq!(b.retained_storage(), (364, 0));
    let reused = b.reserve(340).unwrap();
    assert_eq!(granted.load(AtomicOrdering::Relaxed), 704);
    drop(reused);
    assert_eq!(b.retained_storage(), (364, 0));
    assert_eq!(b.peak(), 704);
}

#[test]
fn retained_transfer_accounts_storage_before_propagating_cancellation() {
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f = flag.clone();
    let b = Budget::new(
        &Limits {
            cancelled: Some(Arc::new(move || f.load(AtomicOrdering::Relaxed))),
            ..Limits::default()
        },
        0,
        None,
    );
    let reservation = b.reserve(256).unwrap();
    flag.store(true, AtomicOrdering::Relaxed);
    assert_eq!(reservation.retain(Some(128)), Err(Error::Cancelled));
    assert_eq!(b.retained_storage(), (128, 0));
    assert_eq!(b.peak(), 256);
    flag.store(false, AtomicOrdering::Relaxed);
    b.poll().unwrap();
    drop(b.reserve(128).unwrap());
    assert_eq!(b.retained_storage(), (128, 0));
    assert_eq!(b.peak(), 256);
}

#[test]
fn a_retained_plan_miss_is_terminal_and_never_requests_postallocation_credit() {
    for actual in [Some(17), None] {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let b = Budget::new(
            &Limits {
                charge: Some(Arc::new(move |_| {
                    c.fetch_add(1, AtomicOrdering::Relaxed);
                    true
                })),
                ..Limits::default()
            },
            0,
            None,
        );
        let reservation = b.reserve(16).unwrap();
        let error = reservation.retain(actual).unwrap_err();
        assert!(matches!(error, Error::Memory { .. }));
        assert_eq!(b.retained_storage(), (actual.unwrap_or(usize::MAX), 0));
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(b.poll(), Err(error.clone()));
        assert!(matches!(b.reserve(1024), Err(Error::Memory { .. })));
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(
            b.peak(),
            16,
            "a failed plan cannot claim additional admitted peak"
        );
    }
}

#[test]
fn accounting_refresh_preserves_admission_and_required_public_polls() {
    let polls = Arc::new(AtomicUsize::new(0));
    let grants = Arc::new(AtomicUsize::new(0));
    let reject = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (p, g, r) = (polls.clone(), grants.clone(), reject.clone());
    let b = Budget::new(
        &Limits {
            cancelled: Some(Arc::new(move || {
                p.fetch_add(1, AtomicOrdering::Relaxed);
                false
            })),
            charge: Some(Arc::new(move |bytes| {
                if r.load(AtomicOrdering::Relaxed) {
                    return false;
                }
                g.fetch_add(bytes, AtomicOrdering::Relaxed);
                true
            })),
            ..Limits::default()
        },
        17,
        None,
    );
    let payload = b.token(80).unwrap();
    let scratch = b.reserve(43).unwrap();
    let live = b.ledger().unwrap().bytes();
    let work_polls = polls.load(AtomicOrdering::Relaxed);
    let (_, sample) = measure(|| b.refresh_base(100).unwrap());
    assert_eq!(sample.events, 0);
    assert_eq!(b.retained_storage(), (117, 43));
    assert_eq!(b.peak(), 117 + 43 + live);
    assert_eq!(grants.load(AtomicOrdering::Relaxed), b.peak());
    assert_eq!(polls.load(AtomicOrdering::Relaxed), work_polls);
    let peak = b.peak();
    b.refresh_base(10).unwrap();
    assert_eq!(b.retained_storage(), (27, 43));
    assert_eq!(b.peak(), peak);
    assert_eq!(grants.load(AtomicOrdering::Relaxed), peak);
    assert!(matches!(
        b.refresh_base(usize::MAX),
        Err(Error::Memory { .. })
    ));
    assert_eq!(b.retained_storage(), (27, 43));
    assert_eq!(polls.load(AtomicOrdering::Relaxed), work_polls);
    reject.store(true, AtomicOrdering::Relaxed);
    let (refused, sample) = measure(|| b.refresh_base(101));
    assert_eq!(
        refused,
        Err(Error::Memory {
            limit: usize::MAX,
            used: 118 + 43 + live
        })
    );
    assert_eq!(sample.events, 0);
    assert_eq!(b.peak(), peak);
    assert_eq!(grants.load(AtomicOrdering::Relaxed), peak);
    assert_eq!(polls.load(AtomicOrdering::Relaxed), work_polls);
    reject.store(false, AtomicOrdering::Relaxed);
    b.refresh_base(101).unwrap();
    assert_eq!(b.peak(), peak + 1);
    assert_eq!(grants.load(AtomicOrdering::Relaxed), peak + 1);
    drop(scratch);
    drop(payload);

    // A hard refusal must include both live payload ownership and concurrent scratch.
    let hard = Budget::new(
        &Limits {
            max_memory: Some(1024),
            ..Limits::default()
        },
        13,
        None,
    );
    let payload = hard.token(80).unwrap();
    let scratch = hard.reserve(29).unwrap();
    let live = hard.ledger().unwrap().bytes();
    let before = hard.peak();
    let base = 1024 - 13 - live - 29 + 1;
    let (refused, sample) = measure(|| hard.refresh_base(base));
    assert_eq!(
        refused,
        Err(Error::Memory {
            limit: 1024,
            used: 1025
        })
    );
    assert_eq!(sample.events, 0);
    assert_eq!(hard.peak(), before);
    assert_eq!(hard.retained_storage(), (13 + base, 29));
    drop(scratch);
    drop(payload);

    // Housekeeping leaves cancellation to the existing public work checkpoints.
    // Real phase admission and scratch allocation still stop before allocating.
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let stopped = Budget::new(
        &Limits {
            cancelled: Some(Arc::new(move || {
                c.fetch_add(1, AtomicOrdering::Relaxed);
                true
            })),
            ..Limits::default()
        },
        0,
        None,
    );
    stopped.refresh_base(64).unwrap();
    assert_eq!(calls.load(AtomicOrdering::Relaxed), 0);
    assert_eq!(stopped.baseline(64), Err(Error::Cancelled));
    let (reservation, sample) = measure(|| stopped.reserve(16));
    assert!(matches!(reservation, Err(Error::Cancelled)));
    assert_eq!(sample.events, 0);
    assert_eq!(calls.load(AtomicOrdering::Relaxed), 2);
    assert_eq!(stopped.retained_storage(), (64, 0));
    assert_eq!(stopped.peak(), 64);
    let expired = Budget::new(
        &Limits {
            deadline: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
            ..Limits::default()
        },
        0,
        None,
    );
    expired.refresh_base(64).unwrap();
    assert_eq!(expired.baseline(64), Err(Error::Timeout));
    assert!(matches!(expired.reserve(16), Err(Error::Timeout)));
    assert_eq!(expired.retained_storage(), (64, 0));

    // A terminal plan miss survives refresh, with no new grant or retained-state reset.
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let terminal = Budget::new(
        &Limits {
            charge: Some(Arc::new(move |_| {
                c.fetch_add(1, AtomicOrdering::Relaxed);
                true
            })),
            ..Limits::default()
        },
        0,
        None,
    );
    let error = terminal.reserve(16).unwrap().retain(Some(17)).unwrap_err();
    assert_eq!(terminal.refresh_base(0), Err(error.clone()));
    assert_eq!(terminal.refresh_base(usize::MAX), Err(error));
    assert_eq!(terminal.retained_storage(), (17, 0));
    assert_eq!(terminal.peak(), 16);
    assert_eq!(calls.load(AtomicOrdering::Relaxed), 1);
}
