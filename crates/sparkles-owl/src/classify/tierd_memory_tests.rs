//! Fail-first counting-allocator, refusal/cleanup, and deterministic-allowance regression
//! tests for roadmap item 1291 (Tier D memory accounting).

use super::dl_clause::{Clause, Lit, Lits, Made, RTerm, Rooted, Store, XLit, hash_table_bytes};
use super::dl_rules::{DlRun, Found};
use super::index::{Axioms, Ex, Index, Interner};
use super::numeric::tests::measure;
use super::saturate::{Concl, Sink, State};
use super::*;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn axioms(body: &str) -> Vec<Axiom> {
    crate::ofn_read::parse(&format!("Prefix(:=<urn:tierd1291:>)\nOntology(\n{body}\n)"))
        .expect("test ontology")
        .axioms
}

/// Adding clauses with distinct head literals, processing them, and triggering high
/// backward subsumption must keep `Store::bytes()` and incremental `Store::add` +
/// `Store::take_grown()` charges at or above the actual heap allocated by `Store`.
#[test]
fn store_add_and_next_cover_counting_allocator_heap_and_subsumption() {
    let mut it = Interner::new();
    let ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
    let mut clauses = Vec::with_capacity(2049);
    for i in 0..2048u32 {
        clauses.push(Clause::new(
            vec![Lit::x(10 + i)],
            vec![Lit::x(5000 + i), Lit::x(10_000)],
        ));
    }
    // Stronger clause that backward-subsumes all 2048 earlier clauses.
    let killer = Clause::new(Vec::new(), vec![Lit::x(10_000)]);
    let mut max = Vec::with_capacity(8);

    let ((store, charged), sample) = measure(|| {
        let mut s = Store::default();
        let mut charged = 0usize;
        for c in clauses {
            charged += s.add(c).expect("not subsumed");
        }
        // Process half the clauses so `by_x` and other processing indexes grow.
        for _ in 0..1024 {
            if s.next(&ix, None, &mut max).is_some() {
                charged += s.take_grown();
            }
        }
        // Backward-subsume all 2048 clauses; dead entries stay allocated in clauses/queue/indexes.
        charged += s.add(killer).expect("stronger clause");
        (s, charged)
    });

    assert!(
        store.clauses.iter().filter(|c| c.dead).count() >= 2048,
        "expected high backward subsumption"
    );
    assert!(
        store.bytes() >= sample.live,
        "Store::bytes() ({}) undercounted live heap ({})",
        store.bytes(),
        sample.live
    );
    assert!(
        charged >= sample.live,
        "incremental Store charges ({charged}) undercounted live heap ({})",
        sample.live
    );
}

/// Large constants-times-TOP rooted initialization must stream rather than building one
/// giant `Found` before checking bounds or work limits.
#[test]
fn dl_root_init_streams_large_constants_times_top_within_budget() {
    let mut body = String::from(
        "SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         SubClassOf(:U0 :Done)\n\
         SubClassOf(:U1 :Done)\n",
    );
    for t in 0..80 {
        body.push_str(&format!("SubClassOf(owl:Thing :T{t})\n"));
    }
    for i in 0..400 {
        body.push_str(&format!("ClassAssertion(:Seed :i{i})\n"));
    }
    let input = axioms(&body);
    let mut k = Classifier::of(&input);

    // A tight work limit must halt early during root init without allocating a giant
    // Found buffer for all 400 * 81 = 32,400 initial conclusions.
    let limits = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        dl_work: Some(256),
        ..Limits::default()
    };
    let (res, sample) = measure(|| k.classify(&limits));
    let out = res.expect("falls back cleanly past dl_work");
    assert!(
        out.stats.dl_rooted_over || out.stats.dl_over_budget,
        "expected rooted work stop: {:?}",
        out.stats
    );
    // 32,400 unstreamed `Found` entries of `(Vec<Lit>, Vec<Lit>)` + interned lits would
    // exceed several megabytes; streamed init stops near 256 clauses.
    assert!(
        out.stats.peak_memory >= sample.peak,
        "peak_memory ({}) undercounted measured peak heap ({})",
        out.stats.peak_memory,
        sample.peak
    );
    // Also verify a tight memory cap stops early inside root init rather than overshooting
    // by megabytes.
    let mut k2 = Classifier::of(&input);
    let cap = 2 << 20;
    let tight = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        max_memory: Some(cap),
        ..Limits::default()
    };
    let (res2, sample2) = measure(|| k2.classify(&tight));
    if let Ok(out2) = res2 {
        assert!(
            out2.stats.peak_memory >= sample2.peak,
            "tight cap peak_memory ({}) < measured peak ({}), live={}, stats={:?}, allow={:?}",
            out2.stats.peak_memory,
            sample2.peak,
            sample2.live,
            out2.stats,
            LAST_ALLOWANCE.get()
        );
    }
    assert!(
        sample2.peak <= cap + (256 << 10),
        "tight cap measured peak ({}) overshot cap ({cap})",
        sample2.peak
    );
}

/// A non-ground `fold` product must charge its `seen` hash table and step-list spare
/// capacity so that its peak reservation covers actual heap allocations.
#[test]
fn non_ground_fold_charges_table_and_step_list_capacity() {
    struct TrackSink<'a> {
        run: &'a DlRun,
        live: usize,
        peak: usize,
    }
    impl Sink for TrackSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn worked(&mut self, _: usize) {}
    }

    let mut it = Interner::new();
    let ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
    let run = DlRun::new(false, Default::default());
    let mut st = State::default();
    // Pre-allocate `ds(&mut st)` outside the measurement so we isolate `fold`'s own heap.
    let _ = super::dl_rules::ds(&mut st);

    let side = |a: Ex| (Vec::new(), vec![Lit::x(a)]);
    let list_a: Vec<_> = (10..74).map(side).collect();
    let list_b: Vec<_> = (200..264).map(side).collect();
    let mut found = Found::default();
    found.products(vec![list_a, list_b], (Vec::new(), Vec::new()));

    let mut sink = TrackSink {
        run: &run,
        live: 0,
        peak: 0,
    };
    let (_, sample) = measure(|| {
        super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, |_, _, _, _| {});
    });
    assert_eq!(
        sink.live, 0,
        "all temporary fold reservations must be released"
    );
    assert!(
        sink.peak >= sample.peak,
        "fold peak reservation ({}) undercounted measured peak heap ({})",
        sink.peak,
        sample.peak
    );
}

/// Refusal from the caller's `charge` callback must stop before large Tier D growth,
/// leave no leaked state, and allow fallback or a subsequent unrestricted classification
/// to succeed cleanly.
#[test]
fn tierd_charge_refusal_cleanup_and_fallback_reclamation() {
    let mut body = String::from(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         SubClassOf(:B :D)\n\
         SubClassOf(:C :D)\n",
    );
    for t in 0..40 {
        body.push_str(&format!("SubClassOf(owl:Thing :P{t})\n"));
    }
    for i in 0..200 {
        body.push_str(&format!("ClassAssertion(:A :ind{i})\n"));
    }
    let input = axioms(&body);

    let mut fresh = Classifier::of(&input);
    let expected = fresh
        .classify(&Limits {
            threads: 1,
            rooted: true,
            dl_steps: true,
            ..Limits::default()
        })
        .expect("unrestricted rooted run");

    // Charge callback grants enough for normalization/base, then refuses mid-saturation.
    let granted = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(AtomicBool::new(false));
    let g = granted.clone();
    let r = refused.clone();
    let cap = 768 << 10;
    let bounded = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        charge: Some(Arc::new(move |bytes| {
            let prev = g.fetch_add(bytes, Ordering::Relaxed);
            if prev.saturating_add(bytes) > cap {
                g.fetch_sub(bytes, Ordering::Relaxed);
                r.store(true, Ordering::Relaxed);
                false
            } else {
                true
            }
        })),
        ..Limits::default()
    };
    let mut classifier = Classifier::of(&input);
    let (res, sample) = measure(|| classifier.classify(&bounded));
    assert!(refused.load(Ordering::Relaxed), "expected charge refusal");
    assert!(
        sample.peak <= cap + (256 << 10),
        "refused run peak heap ({}) overshot charge cap ({cap})",
        sample.peak
    );
    // Whether fallback succeeded within cap or returned Err(Error::Memory), re-running
    // unrestricted on the same Classifier must reclaim cleanly and match fresh output.
    match res {
        Ok(out) => assert!(out.stats.dl_over_budget),
        Err(Error::Memory { .. }) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
    }
    let recovered = classifier
        .classify(&Limits {
            threads: 1,
            rooted: true,
            dl_steps: true,
            ..Limits::default()
        })
        .expect("recovered unrestricted classification");
    assert_eq!(recovered.consistent, expected.consistent);
    assert_eq!(recovered.taxonomy, expected.taxonomy);
    assert_eq!(recovered.stats.memory, expected.stats.memory);
}

/// End-to-end Tier D (rooted and non-rooted, marker repair off and on) charged
/// `stats.peak_memory` must cover actual heap allocated during classification.
#[test]
fn tierd_end_to_end_peak_memory_covers_counting_allocator_heap() {
    let mut body = String::from(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         SubClassOf(:B ObjectSomeValuesFrom(:r :E))\n\
         SubClassOf(:C ObjectSomeValuesFrom(:r :F))\n\
         SubClassOf(:E ObjectUnionOf(:G :H))\n\
         SubClassOf(:F ObjectUnionOf(:G :H))\n\
         SubClassOf(ObjectSomeValuesFrom(:r :G) :Done)\n\
         SubClassOf(ObjectSomeValuesFrom(:r :H) :Done)\n",
    );
    for t in 0..32 {
        body.push_str(&format!("SubClassOf(owl:Thing :Top{t})\n"));
    }
    for i in 0..80 {
        body.push_str(&format!("ClassAssertion(:A :u{i})\n"));
    }
    let rooted_input = axioms(&body);

    for repair in [false, true] {
        let mut k = Classifier::of(&rooted_input);
        let limits = Limits {
            threads: 1,
            rooted: true,
            dl_steps: true,
            rooted_marker_repair: repair,
            ..Limits::default()
        };
        let (out, sample) = measure(|| k.classify(&limits).expect("rooted classify"));
        assert!(
            out.stats.peak_memory >= sample.peak,
            "rooted repair={repair}: peak_memory ({}) < measured peak ({})",
            out.stats.peak_memory,
            sample.peak
        );
    }

    // Non-rooted disjunctive ontology with multiple disjuncts.
    let mut disj_body = String::new();
    for i in 0..24 {
        disj_body.push_str(&format!(
            "SubClassOf(:C{i} ObjectUnionOf(:D{i} :E{i}))\n\
             SubClassOf(:D{i} ObjectSomeValuesFrom(:r :F{i}))\n\
             SubClassOf(:E{i} ObjectSomeValuesFrom(:r :F{i}))\n\
             SubClassOf(:F{i} :Goal)\n"
        ));
    }
    let disj_input = axioms(&disj_body);
    let mut k = Classifier::of(&disj_input);
    let (out, sample) = measure(|| {
        k.classify(&Limits {
            threads: 1,
            ..Limits::default()
        })
        .expect("non-rooted Tier D classify")
    });
    assert!(
        out.stats.peak_memory >= sample.peak,
        "non-rooted Tier D: peak_memory ({}) < measured peak ({})",
        out.stats.peak_memory,
        sample.peak
    );
}

/// `hash_table_bytes` must cover the actual hashbrown allocation across representative
/// entry sizes from the very first entry through repeated rehashes.
#[test]
fn hash_table_bytes_covers_counting_allocator_heap() {
    fn check<V: Default>(label: &str) {
        let entry = std::mem::size_of::<(u32, V)>();
        let mut map: FxHashMap<u32, V> = FxHashMap::default();
        for i in 0..2000u32 {
            let (est, sample) = measure(|| {
                let mut m: FxHashMap<u32, V> = FxHashMap::default();
                for k in 0..=i {
                    m.insert(k, V::default());
                }
                let est = hash_table_bytes(m.capacity(), entry);
                // Keep `m` alive inside `measure` via return? No, return `(m, est)` so
                // `sample.live` measures the map's live heap.
                (m, est)
            });
            let (m, est) = est;
            if i == 0
                || i == 1
                || i == 3
                || i == 4
                || i == 7
                || i == 8
                || i == 15
                || i == 100
                || i == 1999
            {
                assert!(
                    est >= sample.live,
                    "{label} len={}: hash_table_bytes({},{entry})={est} < live={}",
                    m.len(),
                    m.capacity(),
                    sample.live
                );
            }
            map = m;
            if (64..1999).contains(&i) {
                // Step quickly past small sizes to keep the test fast.
                break;
            }
        }
        drop(map);
        // Full sweep 0..2000 in one map measuring cumulative live heap.
        let ((m, _), sample) = measure(|| {
            let mut m: FxHashMap<u32, V> = FxHashMap::default();
            for k in 0..2000u32 {
                m.insert(k, V::default());
            }
            let cap = m.capacity();
            (m, cap)
        });
        let est = hash_table_bytes(m.capacity(), entry);
        assert!(
            est >= sample.live,
            "{label} 2000: hash_table_bytes({},{entry})={est} < live={}",
            m.capacity(),
            sample.live
        );
    }
    check::<()>("u32-set");
    check::<u32>("u32-u32");
    check::<u64>("u32-u64");
    check::<[u64; 2]>("u32-16b");
    check::<Vec<u32>>("u32-vec");
    check::<[u64; 5]>("u32-40b");
}

/// `Store::owned_bytes` (reported by `Store::bytes()`) must match an exact structural scan
/// (`Store::scan_bytes()`) across `add`, `next`, `push_pred`, high backward subsumption,
/// and rooted `ByTerm` / `multi` / `by_xl` / `by_ground` indexing.
#[test]
fn store_owned_bytes_matches_scan_bytes_across_mutations() {
    let mut it = Interner::new();
    let ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
    let lits = Lits::default();
    let inv = |r: u32| Some(r ^ 1);
    let mut s = Store::default();
    assert_eq!(s.bytes(), s.scan_bytes());

    let mut rooted_clauses = Vec::new();
    for i in 0..128u32 {
        let Made::Lit(g1) = lits.lit(XLit::Atom(10 + i, RTerm::C(i % 16)), inv) else {
            panic!()
        };
        let Made::Lit(g2) = lits.lit(XLit::Atom(200 + i, RTerm::C((i + 1) % 16)), inv) else {
            panic!()
        };
        let Made::Lit(eq) = lits.lit(XLit::Eq(RTerm::C(i % 8), RTerm::Fc(3, i % 16)), inv) else {
            panic!()
        };
        rooted_clauses.push(Clause::new(vec![g1], vec![g2, eq]));
    }
    for c in rooted_clauses {
        assert!(s.add(c).is_some());
        assert_eq!(s.bytes(), s.scan_bytes(), "after rooted add");
    }
    let ord = Rooted {
        ix: &ix,
        lits: &lits,
        inv: &inv,
        ground_total: true,
    };
    let mut max = Vec::new();
    for _ in 0..64 {
        let id = s.next(&ix, Some(&ord), &mut max).expect("clause");
        assert_eq!(s.bytes(), s.scan_bytes(), "after rooted next {id}");
    }
    // High backward subsumption via shorter clause.
    let Made::Lit(g1) = lits.lit(XLit::Atom(10, RTerm::C(0)), inv) else {
        panic!()
    };
    assert!(s.add(Clause::new(vec![], vec![g1])).is_some());
    assert_eq!(s.bytes(), s.scan_bytes(), "after backward subsumption");
}

/// Pre-admission refusal on `Store::add_admitted`, `Store::next_admitted`,
/// `Store::push_pred_admitted`, and `Lits::lit_admitted` must leave prior state intact and
/// avoid uncharged heap growth.
#[test]
fn store_and_lits_admitted_refusal_leaves_zero_growth_and_intact_state() {
    struct RefuseSink<'a> {
        run: &'a DlRun,
        allow: bool,
    }
    impl Sink for RefuseSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            bytes == 0 || self.allow
        }
        fn commit_dl(&mut self, _: usize, _: usize) {}
        fn worked(&mut self, _: usize) {}
    }

    let mut it = Interner::new();
    let ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
    let run = DlRun::new(false, Default::default());
    let mut sink = RefuseSink {
        run: &run,
        allow: true,
    };
    let mut store = Store::default();
    // Seed one clause while allowed.
    let c0 = Clause::new(vec![Lit::x(1)], vec![Lit::x(2)]);
    assert!(store.add_admitted(c0, &mut sink));
    let clauses_before = store.clauses.len();
    let bytes_before = store.bytes();

    // Now refuse all growth.
    sink.allow = false;
    let c1 = Clause::new(vec![Lit::x(3)], vec![Lit::x(4)]);
    let (added, add_sample) = measure(|| store.add_admitted(c1, &mut sink));
    assert!(!added, "add_admitted must refuse when sink denies");
    assert!(store.interrupted);
    assert_eq!(store.clauses.len(), clauses_before);
    assert_eq!(store.bytes(), bytes_before);
    assert_eq!(add_sample.live, 0, "refused add must not retain heap");

    // Refused next_admitted leaves clause queued and retains no index growth.
    store.interrupted = false;
    let mut max = Vec::with_capacity(4);
    let (popped, next_sample) = measure(|| store.next_admitted(&ix, None, &mut max, &mut sink));
    assert!(
        popped.is_none(),
        "next_admitted must refuse when sink denies"
    );
    assert!(store.interrupted);
    assert_eq!(store.bytes(), bytes_before);
    assert_eq!(next_sample.live, 0, "refused next must not retain heap");
    assert!(store.has_queued(), "clause must remain queued on refusal");

    // Refused push_pred_admitted leaves preds unchanged.
    store.interrupted = false;
    let (pushed, pred_sample) = measure(|| store.push_pred_admitted(0, &mut sink));
    assert!(!pushed);
    assert!(store.interrupted);
    assert!(store.preds.is_empty());
    assert_eq!(pred_sample.live, 0);

    // Refused Lits::lit_admitted leaves Lits unchanged and allocates nothing.
    let lits = Lits::default();
    let (made, lit_sample) =
        measure(|| lits.lit_admitted(XLit::Atom(7, RTerm::C(9)), |_| None, &mut sink));
    assert!(made.is_none(), "lit_admitted must refuse when sink denies");
    assert!(lits.is_empty());
    assert_eq!(lit_sample.live, 0);
}

/// Separating the logical input-size allowance from actual container capacity makes the
/// discretionary Horn/Tier D memory allowance (`allowance_base` and `own`) identical across
/// `threads = 1` and `threads = 4`, while still charging full actual capacity (`base`)
/// against `Limits::charge`.
#[test]
fn deterministic_allowance_independent_of_thread_count() {
    // Exceed `PARALLEL_AXIOMS` (64) so `threads = 4` takes the parallel chunked normalization
    // and `merge_chunks` path whose container capacities can differ from `threads = 1`.
    let mut body = String::from(
        "SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         SubClassOf(:U0 :Done)\n\
         SubClassOf(:U1 :Done)\n",
    );
    for i in 0..120 {
        body.push_str(&format!(
            "SubClassOf(:C{i} ObjectSomeValuesFrom(:r{} :D{i}))\n\
             SubClassOf(:D{i} :E{i})\n",
            i % 7
        ));
    }
    for i in 0..40 {
        body.push_str(&format!("ClassAssertion(:Seed :ind{i})\n"));
    }
    let input = axioms(&body);

    let run_with = |threads: usize| {
        let charged = Arc::new(AtomicUsize::new(0));
        let ch = charged.clone();
        let mut k = Classifier::of(&input);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                dl_steps: true,
                charge: Some(Arc::new(move |bytes| {
                    ch.fetch_add(bytes, Ordering::Relaxed);
                    true
                })),
                ..Limits::default()
            })
            .expect("classify");
        let last = LAST_ALLOWANCE.get();
        (out, last, charged.load(Ordering::Relaxed))
    };

    let (out1, (base1, logical1, own1), charged1) = run_with(1);
    let (out4, (base4, logical4, own4), charged4) = run_with(4);

    assert_eq!(out1.taxonomy, out4.taxonomy);
    assert_eq!(
        logical1, logical4,
        "logical allowance base must not depend on thread count"
    );
    assert_eq!(
        own1, own4,
        "discretionary allowance `own` must not depend on thread count"
    );
    assert!(
        base1 >= logical1 && base4 >= logical4,
        "capacity-based charge base ({base1}, {base4}) must cover logical base ({logical1}, {logical4})"
    );
    assert!(
        charged1 >= base1 && charged4 >= base4,
        "caller charge callback ({charged1}, {charged4}) must observe full capacity base ({base1}, {base4})"
    );
}
