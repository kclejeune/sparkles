//! The modules' pass of a run that left axioms out admits its index, its extractions and
//! its lists of axioms and classes against the run's memory budget and the pool's grant,
//! as the saturation is admitted (roadmap item 1277). Each test runs the pass through
//! [`AdmittedClassification::cover`] or [`Classification::cover`] and reads what the
//! counting allocator saw, what the limit refused and what the pool was charged.

use super::numeric::tests::measure;
use super::*;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// `n` classes in a shallow hierarchy with long IRIs, and one axiom outside OWL 2 EL that
/// a run with the EL rules alone leaves out. The classes under `:D0` reach it.
fn ontology(n: usize, pad: usize) -> Vec<AxiomKind> {
    let pad = "q".repeat(pad);
    let mut text = format!("Prefix(:=<urn:cov1277-{pad}:>)\nOntology(\n");
    for i in 0..n {
        text.push_str(&format!("SubClassOf(:C{i} :D{})\n", i % 50));
    }
    text.push_str("SubClassOf(:D0 ObjectUnionOf(:U :V))\n)");
    crate::ofn_read::parse(&text)
        .expect("test ontology")
        .axioms
        .into_iter()
        .map(|a| a.kind)
        .collect()
}

fn deferred(max_memory: Option<usize>, partial: bool) -> Limits {
    Limits {
        threads: 1,
        el_only: true,
        cover_later: true,
        partial,
        max_memory,
        ..Limits::default()
    }
}

/// What an unbounded pass over `kinds` holds: the run's held estimate and peak before the
/// pass, the allocator's peak during it, and the answer.
struct Unbounded {
    held: usize,
    peak_before: usize,
    sample: super::numeric::tests::Sample,
    out: Classification,
}

fn unbounded(kinds: &[&AxiomKind]) -> Unbounded {
    let mut sat = Saturation::new();
    let mut admitted = sat
        .classify_admitted(kinds, None, &deferred(None, false))
        .unwrap();
    assert_eq!(sat.excluded().len(), 1);
    let held = admitted.classification().stats.memory;
    let peak_before = admitted.classification().stats.peak_memory;
    let excluded = sat.excluded().to_vec();
    let (r, sample) = measure(|| admitted.cover(kinds, &excluded, None));
    r.unwrap();
    Unbounded {
        held,
        peak_before,
        sample,
        out: admitted.into_classification(),
    }
}

/// The peak the run reports covers every byte the allocator saw the pass hold, and stays
/// within a fiftieth of it.
#[test]
fn the_pass_charges_what_it_holds() {
    let kinds = ontology(400, 256);
    let kinds: Vec<&AxiomKind> = kinds.iter().collect();
    let u = unbounded(&kinds);
    let real = u.sample.peak;
    let list = Classification::incomplete_vec_bytes(&u.out.incomplete);
    assert!(!u.out.incomplete.is_empty() && !u.out.has_all_taxonomy_classes());
    assert_eq!(u.out.stats.memory, u.held + list);
    // the pass holds far more than the list it answers
    assert!(real > 2 * list, "real {real}, list {list}");
    assert!(
        u.out.stats.peak_memory >= u.held + real,
        "the run's peak {} undercounts the pass's {real} beside {}",
        u.out.stats.peak_memory,
        u.held
    );
    let charged = u.out.stats.coverage_memory;
    assert!(
        charged >= real.saturating_sub(list) && charged <= real + real / 50 + 1024,
        "the pass charged {charged} for {real}"
    );
    assert!(u.peak_before < u.held + real);
}

/// A limit that the answer's list fits but the pass's state does not refuses the pass
/// with the peak it would reach, and leaves the answer, the held estimate and the heap as
/// they were, so that the refusal can repeat with the same numbers.
#[test]
fn a_limit_below_the_pass_refuses_it_and_holds_nothing() {
    let kinds = ontology(400, 256);
    let kinds: Vec<&AxiomKind> = kinds.iter().collect();
    let u = unbounded(&kinds);
    let cap = u.held + u.sample.peak - 1;
    let mut sat = Saturation::new();
    let mut admitted = sat
        .classify_admitted(&kinds, None, &deferred(Some(cap), false))
        .expect("the saturation fits below the pass");
    assert_eq!(admitted.classification().stats.memory, u.held);
    let excluded = sat.excluded().to_vec();
    for _ in 0..2 {
        let (r, sample) = measure(|| admitted.cover(&kinds, &excluded, None));
        assert!(
            matches!(r, Err(Error::Memory { limit, used }) if limit == cap && used > cap),
            "{r:?}"
        );
        assert_eq!(sample.live, 0, "a refused pass left {sample:?}");
        let out = admitted.classification();
        assert_eq!(out.stats.memory, u.held);
        assert!(out.incomplete.is_empty() && out.complete_for_taken);
        assert!(!out.stats.coverage_stopped && !out.stats.coverage_over);
        assert!(out.stats.peak_memory <= cap);
    }
}

/// The same refusal through a pool's grant: the pool is never asked past its budget, and
/// what it was charged stays within the run's reported peak.
#[test]
fn a_pool_below_the_pass_refuses_it() {
    let kinds = ontology(400, 256);
    let kinds: Vec<&AxiomKind> = kinds.iter().collect();
    let u = unbounded(&kinds);
    let budget = u.held + u.sample.peak - 1;
    let used = Arc::new(AtomicUsize::new(0));
    let pool = used.clone();
    let limits = Limits {
        charge: Some(Arc::new(move |bytes| {
            let prev = pool.load(Ordering::Relaxed);
            if prev.saturating_add(bytes) > budget {
                return false;
            }
            pool.store(prev + bytes, Ordering::Relaxed);
            true
        })),
        ..deferred(None, false)
    };
    let mut sat = Saturation::new();
    let mut admitted = sat.classify_admitted(&kinds, None, &limits).unwrap();
    let excluded = sat.excluded().to_vec();
    assert!(matches!(
        admitted.cover(&kinds, &excluded, None),
        Err(Error::Memory { used, .. }) if used > budget
    ));
    let charged = used.load(Ordering::Relaxed);
    assert!(charged <= budget);
    assert!(charged <= admitted.classification().stats.peak_memory);
    assert_eq!(admitted.classification().stats.memory, u.held);
}

/// A partial run answers a refused pass as a stopped one: every class incomplete, the
/// consistency unknown and `coverage_stopped` set, once the pass's state is freed, and
/// fails only when the full list is refused too.
#[test]
fn a_partial_run_answers_a_refused_pass_as_stopped() {
    let kinds = ontology(400, 256);
    let kinds: Vec<&AxiomKind> = kinds.iter().collect();
    let u = unbounded(&kinds);
    let mut sat = Saturation::new();
    let full = sat
        .classify_admitted(&kinds, None, &deferred(None, true))
        .unwrap()
        .classification()
        .all_incomplete_bytes()
        .1;
    let cap = u.held + u.sample.peak - 1;
    assert!(
        u.held + full <= cap,
        "the full list {full} must fit under {cap}"
    );
    let mut sat = Saturation::new();
    let mut admitted = sat
        .classify_admitted(&kinds, None, &deferred(Some(cap), true))
        .unwrap();
    let excluded = sat.excluded().to_vec();
    assert_eq!(admitted.cover(&kinds, &excluded, None).unwrap(), None);
    let out = admitted.classification();
    assert!(out.stats.coverage_stopped && !out.complete && out.complete_for_taken);
    assert_eq!(out.consistency, Consistency::Unknown);
    assert!(out.has_all_taxonomy_classes());
    assert_eq!(out.stats.memory, u.held + full);
    assert!(out.stats.peak_memory <= cap);

    // below the full list, the fallback is refused too
    let cap = u.held + full - 1;
    let mut sat = Saturation::new();
    let mut admitted = sat
        .classify_admitted(&kinds, None, &deferred(Some(cap), true))
        .unwrap();
    let excluded = sat.excluded().to_vec();
    assert!(matches!(
        admitted.cover(&kinds, &excluded, None),
        Err(Error::Memory { limit, used }) if limit == cap && used == u.held + full
    ));
    let out = admitted.classification();
    assert_eq!(out.stats.memory, u.held);
    assert!(out.incomplete.is_empty());
}

/// The pass's index polls the deadline and the cancellation as it is built: with no work
/// allowed to the extractions, which then poll nothing, a cancellation still stops the
/// pass, and so does a deadline that has passed.
#[test]
fn the_index_polls_the_stop() {
    let kinds = ontology(9000, 0);
    let kinds: Vec<&AxiomKind> = kinds.iter().collect();
    let armed = Arc::new(AtomicBool::new(false));
    let polls = Arc::new(AtomicUsize::new(0));
    let (a, p) = (armed.clone(), polls.clone());
    let limits = Limits {
        coverage_work: Some(0),
        cancelled: Some(Arc::new(move || {
            if a.load(Ordering::Relaxed) {
                p.fetch_add(1, Ordering::Relaxed);
                true
            } else {
                false
            }
        })),
        ..deferred(None, false)
    };
    let mut sat = Saturation::new();
    let mut admitted = sat.classify_admitted(&kinds, None, &limits).unwrap();
    let held = admitted.classification().stats.memory;
    let excluded = sat.excluded().to_vec();
    armed.store(true, Ordering::Relaxed);
    let (r, sample) = measure(|| admitted.cover(&kinds, &excluded, None));
    assert_eq!(r.unwrap(), None);
    assert_eq!(polls.load(Ordering::Relaxed), 1);
    let out = admitted.into_classification();
    assert!(out.stats.coverage_stopped && !out.stats.coverage_over);
    assert_eq!(out.consistency, Consistency::Unknown);
    assert!(out.has_all_taxonomy_classes());
    // nothing of the pass is left but the full list
    assert_eq!(
        out.stats.memory,
        held + Classification::incomplete_vec_bytes(&out.incomplete)
    );
    assert!(sample.live <= Classification::incomplete_vec_bytes(&out.incomplete));

    // a deadline that has passed, through the unadmitted pass
    let mut sat = Saturation::new();
    let mut out = sat.classify(&kinds, None, &deferred(None, false)).unwrap();
    let late = Limits {
        coverage_work: Some(0),
        deadline: Some(Instant::now()),
        ..deferred(None, false)
    };
    assert_eq!(out.cover(&kinds, sat.excluded(), None, &late), None);
    assert!(out.stats.coverage_stopped && !out.stats.coverage_over);
}

/// A cancellation at any poll of the pass, in the index or in the extractions, leaves
/// every class incomplete and frees the pass's state.
#[test]
fn a_stop_at_any_poll_frees_the_pass() {
    let kinds = ontology(9000, 0);
    let kinds: Vec<&AxiomKind> = kinds.iter().collect();
    let count = |stop_at: Option<usize>| {
        let armed = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let (a, p) = (armed.clone(), polls.clone());
        let limits = Limits {
            cancelled: Some(Arc::new(move || {
                if !a.load(Ordering::Relaxed) {
                    return false;
                }
                let n = p.fetch_add(1, Ordering::Relaxed) + 1;
                stop_at.is_some_and(|k| n >= k)
            })),
            ..deferred(None, false)
        };
        let mut sat = Saturation::new();
        let mut admitted = sat.classify_admitted(&kinds, None, &limits).unwrap();
        let held = admitted.classification().stats.memory;
        let excluded = sat.excluded().to_vec();
        armed.store(true, Ordering::Relaxed);
        let (r, sample) = measure(|| admitted.cover(&kinds, &excluded, None));
        assert_eq!(r.unwrap(), None);
        let out = admitted.into_classification();
        let list = Classification::incomplete_vec_bytes(&out.incomplete);
        assert_eq!(out.stats.memory, held + list);
        assert!(sample.live <= list, "{sample:?} beside a list of {list}");
        (polls.load(Ordering::Relaxed), out)
    };
    let (total, done) = count(None);
    assert!(!done.stats.coverage_stopped);
    // the index polls once for every 4,096 axioms, and the extractions after it
    let index_polls = kinds.len() / 4096;
    assert!(total > index_polls, "{total} polls");
    for k in 1..=total {
        let (_, out) = count(Some(k));
        assert!(out.stats.coverage_stopped, "a stop at poll {k} of {total}");
        assert_eq!(out.consistency, Consistency::Unknown);
        assert!(out.has_all_taxonomy_classes());
    }
}

/// The meter's estimate of the pass, read beside the allocator inside the pass itself:
/// its peak covers the allocator's and stays within a fiftieth of it, and what it still
/// counts when the pass returns covers the list it answers.
#[test]
fn the_meter_is_tight_against_the_allocator() {
    for (n, pad) in [(400, 256), (3000, 8), (50, 0), (20000, 40)] {
        let kinds = ontology(n, pad);
        let kinds: Vec<&AxiomKind> = kinds.iter().collect();
        let mut sat = Saturation::new();
        let out = sat.classify(&kinds, None, &deferred(None, false)).unwrap();
        let left: Vec<&AxiomKind> = sat.excluded().iter().map(|e| &e.axiom).collect();
        let ((c, peak, live), sample) = measure(|| {
            let meter = crate::transform::Meter::new(None);
            let classes = by_depth(&out.taxonomy, &meter).unwrap();
            let c = crate::transform::coverage_refs(
                &kinds,
                &left,
                true,
                classes,
                usize::MAX,
                &|| false,
                &meter,
            )
            .unwrap();
            (c, meter.peak(), meter.live())
        });
        assert!(!c.incomplete.is_empty());
        assert!(
            peak >= sample.peak && peak <= sample.peak + sample.peak / 50 + 1024,
            "{n}x{pad}: the meter's peak {peak} against the allocator's {}",
            sample.peak
        );
        assert!(
            live >= sample.live,
            "{n}x{pad}: the meter still counts {live} of {}",
            sample.live
        );
        drop(c);
    }
}
