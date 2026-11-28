//! Fail-first counting-allocator, refusal/cleanup, and deterministic-allowance regression
//! tests for roadmap item 1291 (Tier D memory accounting).

use super::dl_clause::{
    BEFORE_LIT_REVALIDATE, Clause, Lit, Lits, Made, RTerm, Rooted, Store, XLit, hash_table_bytes,
};
use super::dl_rules::{Deriver, DlRun, Found, MREPAIR_ON, NoopDerive};
use super::index::{Axioms, Ex, Index, Interner};
use super::numeric::tests::{
    measure, sample, take_watched_deallocations, watch_deallocations,
    watched_deallocations_distinct,
};
use super::saturate::{Concl, Sink, State};
use super::*;
use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

fn axioms(body: &str) -> Vec<Axiom> {
    crate::ofn_read::parse(&format!("Prefix(:=<urn:tierd1291:>)\nOntology(\n{body}\n)"))
        .expect("test ontology")
        .axioms
}

fn rooted_index(body: &str) -> (Index, Ex, usize) {
    let input = axioms(body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let allowed = super::dl_norm::Allowed {
        rooted: true,
        ..Default::default()
    };
    let mut dl = super::dl_norm::normalize(&outside, &mut it, &mut ax, None, &|| false, allowed)
        .expect("dl_norm")
        .map(|(_, d)| d)
        .unwrap_or_default();
    let root = dl.root.unwrap_or_else(|| {
        let r = it.fresh_class();
        dl.root = Some(r);
        r
    });
    for set in &mut ax.disjoint {
        set.sort_unstable();
    }
    ax.dl = Some(Box::new(dl));
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    (ix, root, it.nodes.len())
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
        super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, &NoopDerive);
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
        for i in [0u32, 1, 3, 4, 7, 8, 15, 31, 63, 100] {
            let ((m, est), sample) = measure(|| {
                let mut m: FxHashMap<u32, V> = FxHashMap::default();
                for k in 0..=i {
                    m.insert(k, V::default());
                }
                let est = hash_table_bytes(m.capacity(), entry);
                (m, est)
            });
            assert!(
                est >= sample.live,
                "{label} len={}: hash_table_bytes({},{entry})={est} < live={}",
                m.len(),
                m.capacity(),
                sample.live
            );
            map = m;
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
/// against `Limits::charge`. Uses >2,500 axioms so `threads = 4` exceeds `MIN_AXIOMS` (1,024)
/// and genuinely splits normalization across multiple worker chunks.
#[test]
fn deterministic_allowance_independent_of_thread_count() {
    let build = |disj: bool| {
        let mut body = if disj {
            String::from(
                "SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
                 SubClassOf(:U0 :Done)\n\
                 SubClassOf(:U1 :Done)\n",
            )
        } else {
            String::from(
                "InverseObjectProperties(:r0 :inv0)\n\
                 SubClassOf(:Seed ObjectAllValuesFrom(:r0 :Done))\n",
            )
        };
        for i in 0..1250 {
            body.push_str(&format!(
                "SubClassOf(:C{i} ObjectSomeValuesFrom(:r{} :D{i}))\n\
                 SubClassOf(:D{i} :E{i})\n",
                i % 7
            ));
        }
        if disj {
            for i in 0..40 {
                body.push_str(&format!("ClassAssertion(:Seed :ind{i})\n"));
            }
        }
        axioms(&body)
    };

    for (label, disj, rooted) in [("horn", false, false), ("rooted-tierd", true, true)] {
        let input = build(disj);
        assert!(
            input.len() >= 2500,
            "{label}: fixture must exceed 2 * MIN_AXIOMS (got {})",
            input.len()
        );

        let run_with = |threads: usize| {
            let charged = Arc::new(AtomicUsize::new(0));
            let ch = charged.clone();
            let mut k = Classifier::of(&input);
            let out = k
                .classify(&Limits {
                    threads,
                    rooted,
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

        assert_eq!(out1.taxonomy, out4.taxonomy, "{label}: taxonomy mismatch");
        assert_eq!(
            logical1, logical4,
            "{label}: logical allowance base must not depend on thread count"
        );
        assert_eq!(
            own1, own4,
            "{label}: discretionary allowance `own` must not depend on thread count"
        );
        assert!(
            base1 >= logical1 && base4 >= logical4,
            "{label}: capacity-based charge base ({base1}, {base4}) must cover logical base ({logical1}, {logical4})"
        );
        assert!(
            charged1 >= base1 && charged4 >= base4,
            "{label}: caller charge callback ({charged1}, {charged4}) must observe full capacity base ({base1}, {base4})"
        );
    }
}

/// Shared literal admission (`Lits::lit_admitted`) must revalidate under the interner mutex
/// across both segment-allocation (1,024 / 3,072) and hash-rehash boundaries, publishing
/// neither a literal nor a retained allocation when a retry is refused.
#[test]
fn lits_admitted_revalidates_across_segment_and_rehash_boundaries() {
    struct CapSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        stopped: bool,
    }
    impl Sink for CapSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            true
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    let run = DlRun::new(false, Default::default());
    let inv = |_: u32| None;

    // Deterministic 2-barrier race using `on_first_admit` (b1) + `BEFORE_LIT_REVALIDATE` (b2):
    struct BarrierSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        stopped: bool,
        on_first_admit: Option<Arc<Barrier>>,
    }
    impl Sink for BarrierSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            if let Some(b) = self.on_first_admit.take() {
                b.wait();
            }
            true
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    // Boundary A (segment boundary 1,023 -> 1,024): A prepares a 1,024-bucket map rehash at
    // len=448 (smaller than segment 1's 2,048-slot allocation); B fills to 1,024 so A's retry
    // hits the segment-1 boundary and is refused.
    {
        let lits = Lits::default();
        for i in 0..448u32 {
            lits.lit(XLit::Atom(i, RTerm::C(1)), inv);
        }
        let entry = std::mem::size_of::<(XLit, u32)>();
        let map_rehash = hash_table_bytes(896, entry);
        let seg1_bytes = 2048 * std::mem::size_of::<std::sync::OnceLock<XLit>>();
        assert!(
            seg1_bytes > map_rehash,
            "expected seg1 ({seg1_bytes}) > 1024-bucket rehash ({map_rehash})"
        );
        let b1 = Arc::new(Barrier::new(2));
        let b2 = Arc::new(Barrier::new(2));
        let mut sink = BarrierSink {
            run: &run,
            cap: map_rehash,
            live: 0,
            stopped: false,
            on_first_admit: Some(b1.clone()),
        };
        let target = XLit::Atom(999_001, RTerm::C(9));
        let (made, bytes_after_b, len_after_b) = std::thread::scope(|s| {
            let b = s.spawn(|| {
                b1.wait();
                for i in 448..1024u32 {
                    lits.lit(XLit::Atom(i, RTerm::C(1)), inv);
                }
                let snap = (lits.bytes(), lits.len());
                b2.wait();
                snap
            });
            BEFORE_LIT_REVALIDATE.with(|slot| *slot.borrow_mut() = Some(b2.clone()));
            let made = lits.lit_admitted(target, inv, &mut sink);
            BEFORE_LIT_REVALIDATE.with(|slot| *slot.borrow_mut() = None);
            let (bb, lb) = b.join().unwrap();
            (made, bb, lb)
        });
        assert!(
            made.is_none(),
            "refused segment-boundary retry must return None"
        );
        assert_eq!(
            lits.len(),
            len_after_b,
            "refused retry must not publish literal"
        );
        assert_eq!(
            lits.bytes(),
            bytes_after_b,
            "refused retry must not retain segment allocation"
        );
        assert_eq!(
            sink.live, 0,
            "prior map_rehash debit must roll back on refused retry"
        );
    }

    // Boundary B (hash rehash boundary): A prepares segment 1 at len=1024; B fills to 1792
    // (allocating segment 1 and filling the hash table so a 4096-bucket rehash is needed).
    {
        let lits = Lits::default();
        for i in 0..1024u32 {
            lits.lit(XLit::Atom(i, RTerm::C(2)), inv);
        }
        let seg1_bytes = 2048 * std::mem::size_of::<std::sync::OnceLock<XLit>>();
        let b1 = Arc::new(Barrier::new(2));
        let b2 = Arc::new(Barrier::new(2));
        let mut sink = BarrierSink {
            run: &run,
            cap: seg1_bytes, // fits seg1 ONLY, refuses 4096-bucket hash rehash
            live: 0,
            stopped: false,
            on_first_admit: Some(b1.clone()),
        };
        let target = XLit::Atom(999_002, RTerm::C(9));
        let (made, bytes_after_b, len_after_b) = std::thread::scope(|s| {
            let b = s.spawn(|| {
                b1.wait();
                for i in 1024..1792u32 {
                    lits.lit(XLit::Atom(i, RTerm::C(2)), inv);
                }
                let snap = (lits.bytes(), lits.len());
                b2.wait();
                snap
            });
            BEFORE_LIT_REVALIDATE.with(|slot| *slot.borrow_mut() = Some(b2.clone()));
            let made = lits.lit_admitted(target, inv, &mut sink);
            BEFORE_LIT_REVALIDATE.with(|slot| *slot.borrow_mut() = None);
            let (bb, lb) = b.join().unwrap();
            (made, bb, lb)
        });
        assert!(
            made.is_none(),
            "refused rehash-boundary retry must return None"
        );
        assert_eq!(
            lits.len(),
            len_after_b,
            "refused retry must not publish literal"
        );
        assert_eq!(
            lits.bytes(),
            bytes_after_b,
            "refused retry must not retain rehashed table"
        );
        assert_eq!(
            sink.live, 0,
            "prior seg1 debit must roll back on refused retry"
        );
    }

    // Stopped sink must refuse even when `need == 0`.
    {
        let lits = Lits::default();
        lits.lit(XLit::Atom(1, RTerm::C(1)), inv);
        let mut sink = CapSink {
            run: &run,
            cap: usize::MAX,
            live: 0,
            stopped: true,
        };
        assert!(
            lits.lit_admitted(XLit::Atom(2, RTerm::C(1)), inv, &mut sink)
                .is_none()
        );
        assert_eq!(lits.len(), 1);
    }
}

/// Concurrent `Out::commit_dl` and `admit_lit` publication must not double-count retained
/// growth against the temporary debit when another worker checks memory simultaneously.
#[test]
fn concurrent_tierd_commit_does_not_double_count_under_tight_cap() {
    let (ok, peak, cap) = super::saturate::test_concurrent_commit_dl_no_double_count();
    assert!(
        ok,
        "concurrent commit_dl/admit_lit tripped tight cap (peak={peak}, cap={cap})"
    );
    assert!(
        peak <= cap,
        "concurrent commit_dl/admit_lit inflated peak ({peak}) past tight cap ({cap})"
    );
}

/// Wide clause construction (`Store::add_parts_admitted_covered`) must reuse unique empty-body
/// buffers without copying, refuse duplicated copy scratch before allocating, and rooted
/// `Store::next_admitted` must refuse `maximal_rooted` views/`max` scratch before allocating
/// while keeping `owned_bytes == scan_bytes()` even with dead queue entries ahead.
#[test]
fn store_add_parts_and_rooted_next_refuse_copy_and_maxima_scratch() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.live.saturating_add(bytes) > self.cap {
                return false;
            }
            self.live += bytes;
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn worked(&mut self, _: usize) {}
    }

    let mut it = Interner::new();
    let ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
    let run = DlRun::new(false, Default::default());

    // 1. Unique wide empty-body head: `covered` covers the source buffer; cap admits index
    // growth only (NOT a second 512 * 8 = 4 KiB copy).
    let mut store = Store::default();
    let wide_unique: Vec<Lit> = (10..522u32).map(Lit::x).collect();
    let head_bytes = wide_unique.len() * std::mem::size_of::<Lit>();
    // Measure exact index bound via a dry probe store.
    let mut probe = Store::default();
    let mut unlimited = BudgetSink {
        run: &run,
        cap: usize::MAX,
        live: 0,
    };
    assert!(probe.add_parts_admitted(Vec::new(), wide_unique.clone(), &mut unlimited));
    let exact_needed = probe.bytes();

    // Cap fits `exact_needed` + generous slack for power-of-2 index rehashes, but NOT +4096
    // extra for a second copy: actually verify duplicated head fails at a tighter cap.
    let mut sink_dup = BudgetSink {
        run: &run,
        cap: head_bytes + 64, // admits source buffer, refuses copy scratch (2048 B)
        live: head_bytes,     // caller already holds `head_bytes`
    };
    let mut wide_dup = Vec::with_capacity(512);
    for i in 0..256u32 {
        wide_dup.push(Lit::x(10 + i));
        wide_dup.push(Lit::x(10 + i));
    }
    let (ok, consumed) =
        store.add_parts_admitted_covered(Vec::new(), wide_dup, head_bytes, &mut sink_dup);
    assert!(
        !ok && !consumed,
        "duplicated wide head must refuse copy scratch"
    );
    assert!(store.interrupted);
    assert!(store.clauses.is_empty());
    assert_eq!(store.bytes(), store.scan_bytes());

    // 2. Rooted `next_admitted` with a dead shorter queue entry ahead of a live wide 256-literal
    // clause: dead-entry cleanup discards the shorter dead entry without allocating, then the
    // cap refuses `maximal_rooted`'s `views` (`256 * size_of::<XLit>()`) before allocation.
    let inv = |_: u32| None;
    let lits = Lits::default();
    let mut rooted_store = Store::default();
    let mut wide_head = Vec::with_capacity(256);
    for i in 0..256u32 {
        let Made::Lit(l) = lits.lit(XLit::Atom(10 + i, RTerm::C(i % 8)), inv) else {
            panic!()
        };
        wide_head.push(l);
    }
    assert!(rooted_store.add_parts_admitted(vec![wide_head[0]], wide_head.clone(), &mut unlimited));
    // Add a dead 2-literal clause ahead of the 257-literal live clause: add `[extra, extra2]`,
    // backward-subsume it with unit `[extra]`, then process `[extra]` so the dead 2-literal
    // entry sits at the very front of `queue`.
    let Made::Lit(extra) = lits.lit(XLit::Atom(999, RTerm::C(1)), inv) else {
        panic!()
    };
    let Made::Lit(extra2) = lits.lit(XLit::Atom(1000, RTerm::C(1)), inv) else {
        panic!()
    };
    assert!(rooted_store.add_parts_admitted(vec![], vec![extra, extra2], &mut unlimited));
    assert!(rooted_store.add_parts_admitted(vec![], vec![extra], &mut unlimited));
    assert!(rooted_store.clauses.iter().any(|s| s.dead));
    let ord = Rooted {
        ix: &ix,
        lits: &lits,
        inv: &inv,
        ground_total: true,
    };
    let mut drain_unit = Vec::new();
    let unit_id = rooted_store
        .next_admitted(&ix, Some(&ord), &mut drain_unit, &mut unlimited)
        .expect("pop live unit [extra] first");
    assert_eq!(rooted_store.clauses[unit_id as usize].c.head(), &[extra]);
    let bytes_before = rooted_store.bytes();

    let mut tiny_sink = BudgetSink {
        run: &run,
        cap: 16, // far below `views` (`256 * size_of::<XLit>()`)
        live: 0,
    };
    let mut max = Vec::new();
    let (popped, sample) =
        measure(|| rooted_store.next_admitted(&ix, Some(&ord), &mut max, &mut tiny_sink));
    assert!(
        popped.is_none(),
        "next_admitted must refuse maximal_rooted scratch"
    );
    assert!(rooted_store.interrupted);
    assert_eq!(rooted_store.bytes(), bytes_before);
    assert_eq!(rooted_store.bytes(), rooted_store.scan_bytes());
    assert!(rooted_store.has_queued(), "live clause must remain queued");
    assert_eq!(
        rooted_store.peek_live(&run),
        Some(0),
        "dead shorter clause must have been discarded ahead of live clause 0"
    );
    assert_eq!(
        max.capacity(),
        0,
        "max must not allocate when scratch is refused"
    );
    assert_eq!(sample.live, 0, "refused rooted next must retain zero heap");
    let _ = exact_needed;
}

/// Non-ground `fold` with wide unions, the `FOLD_PRUNE` streaming transition, and wide
/// unions under spare capacity (on both non-ground and ground paths) must keep every live
/// buffer charged through its last use and stop immediately on refusal. Since item 1280 no
/// caller reserves a bound on fold's tables before it drains, so fold's own charge covers
/// them.
#[test]
fn fold_charges_wide_unions_prune_streaming_and_wide_spare_capacity() {
    struct TrackSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for TrackSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    let (ix, _, _) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let run = DlRun::new(false, Default::default());

    // A. Large wide unions: fold's own reservation covers the heap on both growth and
    // prune paths, with nothing reserved before the drain.
    {
        let mut st = State::default();
        let _ = super::dl_rules::ds(&mut st);
        let wide = |base: Ex| (Vec::new(), (base..base + 256).map(Lit::x).collect());
        let list_a: Vec<_> = (0..16u32).map(|i| wide(100 + i * 300)).collect();
        let list_b: Vec<_> = (0..8u32).map(|i| wide(10_000 + i * 300)).collect();
        let mut found = Found::default();
        found.products(vec![list_a, list_b], (Vec::new(), Vec::new()));
        let mut sink = TrackSink {
            run: &run,
            cap: usize::MAX,
            live: 0,
            peak: 0,
            stopped: false,
        };
        let (_, sample) = measure(|| {
            super::dl_rules::drain(&ix, &mut st, found, &mut sink);
        });
        assert!(
            sink.peak >= sample.peak,
            "fold peak ({}) undercounted heap ({})",
            sink.peak,
            sample.peak
        );
        // After drain, only retained Store growth remains in `sink.live`.
        let store_bytes = super::dl_rules::dl(&st).map_or(0, |d| d.store.bytes());
        assert_eq!(
            sink.live, store_bytes,
            "fold must release all temporary reservations"
        );

        // A tight cap covering `held` + 64 KiB (not enough for 128 wide unions) must refuse
        // before allocating all unions.
        let mut st2 = State::default();
        let _ = super::dl_rules::ds(&mut st2);
        let list_a2: Vec<_> = (0..16u32).map(|i| wide(100 + i * 300)).collect();
        let list_b2: Vec<_> = (0..8u32).map(|i| wide(10_000 + i * 300)).collect();
        let mut found2 = Found::default();
        found2.products(vec![list_a2, list_b2], (Vec::new(), Vec::new()));
        let held2 = found2.held_bytes();
        let mut tight = TrackSink {
            run: &run,
            cap: held2 + (64 << 10),
            live: 0,
            peak: 0,
            stopped: false,
        };
        super::dl_rules::drain(&ix, &mut st2, found2, &mut tight);
        assert!(tight.stopped, "tight cap must refuse wide fold");
        assert!(st2.interrupted);
        assert_eq!(tight.live, 0);
    }

    // B. `FOLD_PRUNE` (4,096) streaming transition:
    // 1. `drained` stays charged during emission;
    // 2. Stopping on the 5th emitted item halts immediately (does not emit all 4,096).
    {
        struct ObserveDerive {
            count: Arc<AtomicUsize>,
            min_live: Arc<AtomicUsize>,
            stop_at: Option<usize>,
            live_ref: Arc<AtomicUsize>,
            stop_flag: Arc<AtomicBool>,
        }
        impl Deriver for ObserveDerive {
            fn derive<S: Sink>(
                &self,
                _: &Index,
                _: &mut State,
                _: super::dl_rules::Side,
                _: &mut S,
            ) {
                let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
                let cur = self.live_ref.load(Ordering::Relaxed);
                self.min_live.fetch_min(cur, Ordering::Relaxed);
                if self.stop_at == Some(n) {
                    self.stop_flag.store(true, Ordering::Relaxed);
                }
            }
        }
        struct SharedSink<'a> {
            run: &'a DlRun,
            live: Arc<AtomicUsize>,
            stopped: Arc<AtomicBool>,
        }
        impl Sink for SharedSink<'_> {
            fn send(&mut self, _: Ex, _: Concl) {}
            fn me(&mut self, _: Concl) {}
            fn dl(&self) -> &DlRun {
                self.run
            }
            fn reserve_data(&mut self, bytes: usize) -> bool {
                if self.stopped.load(Ordering::Relaxed) {
                    return false;
                }
                self.live.fetch_add(bytes, Ordering::Relaxed);
                true
            }
            fn release_data(&mut self, bytes: usize) {
                self.live.fetch_sub(bytes, Ordering::Relaxed);
            }
            fn stopped(&self) -> bool {
                self.stopped.load(Ordering::Relaxed)
            }
            fn go_on(&self) -> bool {
                !self.stopped()
            }
            fn worked(&mut self, _: usize) {}
        }

        let side = |a: Ex| (Vec::new(), vec![Lit::x(a)]);
        let list_a: Vec<_> = (10..75).map(side).collect(); // 65
        let list_b: Vec<_> = (200..265).map(side).collect(); // 65 -> 4225 > FOLD_PRUNE (4096)
        let mut found = Found::default();
        found.products(
            vec![list_a.clone(), list_b.clone()],
            (Vec::new(), Vec::new()),
        );

        let count = Arc::new(AtomicUsize::new(0));
        let min_live = Arc::new(AtomicUsize::new(usize::MAX));
        let live_ref = Arc::new(AtomicUsize::new(0));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let deriver = ObserveDerive {
            count: count.clone(),
            min_live: min_live.clone(),
            stop_at: Some(5),
            live_ref: live_ref.clone(),
            stop_flag: stop_flag.clone(),
        };
        let mut sink = SharedSink {
            run: &run,
            live: live_ref.clone(),
            stopped: stop_flag.clone(),
        };
        let mut st = State::default();
        super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, &deriver);
        assert_eq!(
            count.load(Ordering::Relaxed),
            5,
            "FOLD_PRUNE drain loop must halt immediately when stopped"
        );
        // 4,096 Side structs (48 B each = 196,608 B) must still be charged during emission.
        assert!(
            min_live.load(Ordering::Relaxed) >= 4096 * std::mem::size_of::<super::dl_rules::Side>(),
            "drained buffer was released before emission finished (min_live={})",
            min_live.load(Ordering::Relaxed)
        );
        assert_eq!(live_ref.load(Ordering::Relaxed), 0);
        assert!(st.interrupted);
    }

    // C. Wide unions under spare capacity on ground product path (16 choices << PRODUCT_POLL):
    // must reserve `at` + candidate + key payload before inserting and cover heap.
    {
        let mut st = State::default();
        let _ = super::dl_rules::ds(&mut st);
        let wide = |base: Ex| (Vec::new(), (base..base + 512).map(Lit::x).collect());
        let list_a: Vec<_> = (0..4u32).map(|i| wide(100 + i * 600)).collect();
        let list_b: Vec<_> = (0..4u32).map(|i| wide(10_000 + i * 600)).collect();
        let mut found = Found::default();
        found.ground_products(vec![list_a, list_b], (Vec::new(), Vec::new()));
        let mut sink = TrackSink {
            run: &run,
            cap: usize::MAX,
            live: 0,
            peak: 0,
            stopped: false,
        };
        let (_, sample) = measure(|| {
            super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, &NoopDerive);
        });
        assert_eq!(sink.live, 0, "ground product must release all reservations");
        assert!(
            sink.peak >= sample.peak,
            "ground product peak ({}) undercounted heap ({})",
            sink.peak,
            sample.peak
        );

        // Tight cap refusing the 5th wide choice must stop early before choice 256.
        let mut st2 = State::default();
        let _ = super::dl_rules::ds(&mut st2);
        let list_a2: Vec<_> = (0..4u32).map(|i| wide(100 + i * 600)).collect();
        let list_b2: Vec<_> = (0..4u32).map(|i| wide(10_000 + i * 600)).collect();
        let mut found2 = Found::default();
        found2.ground_products(vec![list_a2, list_b2], (Vec::new(), Vec::new()));
        let held2 = found2.held_bytes();
        let mut tight = TrackSink {
            run: &run,
            cap: held2 + (48 << 10),
            live: 0,
            peak: 0,
            stopped: false,
        };
        super::dl_rules::drain_with(&ix, &mut st2, found2, &mut tight, &NoopDerive);
        assert!(
            tight.stopped,
            "ground product must refuse wide spare-capacity growth"
        );
        assert!(st2.interrupted);
        assert_eq!(tight.live, 0);
    }
}

/// Repeated `redge` + `rreach` on `MRep::regs` must never trigger a tombstone-driven rehash
/// panic (`kept <= admitted`) and must credit freed registration vectors back after replay.
#[test]
fn mrep_regs_rreach_replay_avoids_tombstone_rehash_and_credits_freed_vec() {
    struct DlLedgerSink<'a> {
        run: &'a DlRun,
        live: usize,
        credited: usize,
    }
    impl Sink for DlLedgerSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.live += bytes;
            true
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            assert!(
                kept <= admitted,
                "MRep::regs growth {kept} exceeded admitted bound {admitted}"
            );
            self.live -= admitted - kept;
        }
        fn credit_dl(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
            self.credited += bytes;
        }
        fn worked(&mut self, _: usize) {}
    }

    let (ix, root, _) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let mut run = DlRun::new(false, Default::default());
    run.mrepair = MREPAIR_ON;
    let mut sink = DlLedgerSink {
        run: &run,
        live: 0,
        credited: 0,
    };
    let mut st = State::default();

    // Exercise the exact tombstone pattern (insert 14, replay 3, insert 3, repeat 150x)
    // with multi-constant registrations per context so inner Vecs allocate and free.
    for round in 0..30u32 {
        let base_ctx = 100 + round * 14;
        for j in 0..14u32 {
            let ctx = base_ctx + j;
            for c in 0..8u32 {
                assert!(super::dl_root::redge(&mut st, ctx, c, &mut sink));
            }
        }
        for j in 0..14u32 {
            let ctx = base_ctx + j;
            assert!(super::dl_root::rreach(&ix, root, &mut st, ctx, &mut sink));
        }
    }
    assert!(
        sink.credited >= 30 * 14 * 8 * std::mem::size_of::<u32>(),
        "rreach must credit freed regs vectors back (credited={})",
        sink.credited
    );
    let dl_bytes = super::dl_rules::dl(&st).expect("dl state").bytes() + run.mtab_bytes();
    assert_eq!(
        sink.live,
        std::mem::size_of::<super::saturate::Extra>() + dl_bytes,
        "retained ledger ({}) must equal actual Extra + DlSt bytes ({})",
        sink.live,
        std::mem::size_of::<super::saturate::Extra>() + dl_bytes
    );
}

/// A panicking caller callback while temporary reservations are active must unwind cleanly
/// and release every granted temporary debit in `dl_root::init`, `fold`, ground product, and
/// `rret`.
#[test]
fn callback_panic_unwinds_and_releases_temporary_reservations() {
    struct PanicOnPollSink<'a> {
        run: &'a DlRun,
        live: Arc<AtomicUsize>,
        panic_on_admit_after: Option<usize>,
        admits: usize,
        panic_on_go_on: bool,
    }
    impl Sink for PanicOnPollSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            self.admits += 1;
            if self.panic_on_admit_after == Some(self.admits) {
                panic!("simulated callback panic during reservation");
            }
            self.live.fetch_add(bytes, Ordering::Relaxed);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live.fetch_sub(bytes, Ordering::Relaxed);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live
                .fetch_sub(admitted.saturating_sub(kept), Ordering::Relaxed);
        }
        fn go_on(&self) -> bool {
            if self.panic_on_go_on {
                panic!("simulated callback panic during go_on");
            }
            true
        }
        fn worked(&mut self, _: usize) {}
    }

    let mut it = Interner::new();
    let ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
    let run = DlRun::new(false, Default::default());

    // 1. Panic inside `fold` after `held_guard` and `fold` reservations succeeded.
    {
        let live = Arc::new(AtomicUsize::new(0));
        let side = |a: Ex| (Vec::new(), vec![Lit::x(a)]);
        let list_a: Vec<_> = (10..30).map(side).collect();
        let list_b: Vec<_> = (100..120).map(side).collect();
        let mut found = Found::default();
        found.products(vec![list_a, list_b], (Vec::new(), Vec::new()));
        let mut st = State::default();
        let mut sink = PanicOnPollSink {
            run: &run,
            live: live.clone(),
            panic_on_admit_after: None,
            admits: 0,
            panic_on_go_on: true,
        };
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, &NoopDerive);
        }));
        assert!(res.is_err(), "expected simulated go_on panic");
        assert_eq!(
            live.load(Ordering::Relaxed),
            0,
            "fold/drain must release all reservations on unwind"
        );
    }

    // 2. Panic inside ground product after `held_guard`, `at`, and `seen` reservations succeeded.
    {
        let live = Arc::new(AtomicUsize::new(0));
        let side = |a: Ex| (Vec::new(), vec![Lit::x(a)]);
        let list_a: Vec<_> = (10..30).map(side).collect();
        let list_b: Vec<_> = (100..120).map(side).collect();
        let mut found = Found::default();
        found.ground_products(vec![list_a, list_b], (Vec::new(), Vec::new()));
        let mut st = State::default();
        let mut sink = PanicOnPollSink {
            run: &run,
            live: live.clone(),
            panic_on_admit_after: None,
            admits: 0,
            panic_on_go_on: true,
        };
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, &NoopDerive);
        }));
        assert!(res.is_err(), "expected simulated go_on panic");
        assert_eq!(
            live.load(Ordering::Relaxed),
            0,
            "ground product must release all reservations on unwind"
        );
    }

    // 3. Panic inside `dl_root::init` while `head_guard` holds `head_bytes` for a wide TOP clause.
    {
        let mut body = String::from("SubClassOf(owl:Thing ObjectUnionOf(");
        for i in 0..32 {
            body.push_str(&format!(":D{i} "));
        }
        body.push_str("))\nClassAssertion(:D0 :u0)\n");
        let (ix2, _, _) = rooted_index(&body);
        let run2 = DlRun::new(false, Default::default());
        let live = Arc::new(AtomicUsize::new(0));
        let mut st = State::default();
        let mut sink = PanicOnPollSink {
            run: &run2,
            live: live.clone(),
            panic_on_admit_after: Some(5), // panics after DlSt/RootSt + head_bytes + a lit
            admits: 0,
            panic_on_go_on: false,
        };
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::dl_root::init(&ix2, &mut st, &mut sink);
        }));
        assert!(
            res.is_err(),
            "expected simulated admit panic in dl_root::init"
        );
        // Only the committed DlSt + RootSt + already-interned Lits bytes may remain, NOT
        // `head_bytes` (`32 * 8 = 256 B`) from the unwound TOP clause.
        let committed_st = std::mem::size_of::<super::saturate::Extra>()
            + super::dl_rules::dl(&st).map_or(0, |d| d.bytes())
            + run2.mtab_bytes()
            + run2.lits.bytes();
        assert_eq!(
            live.load(Ordering::Relaxed),
            committed_st,
            "dl_root::init leaked head_bytes reservation on unwind"
        );
    }

    // 4. Panic inside `dl_root::rret` while `guard` holds `slack`.
    {
        let (ix3, _, _) = rooted_index(
            "SubClassOf(:A ObjectUnionOf(:B :C))\n\
             ClassAssertion(:A :u0)\n",
        );
        let run3 = DlRun::new(false, Default::default());
        let inv = |_: u32| None;
        let Made::Lit(g0) = run3.lits.lit(XLit::Atom(10, RTerm::C(0)), inv) else {
            panic!()
        };
        let Made::Lit(g1) = run3.lits.lit(XLit::Atom(11, RTerm::C(0)), inv) else {
            panic!()
        };
        let cid = run3.table.push(Clause::new(vec![g0, g1], vec![Lit::x(20)]));
        let mut st = State::default();
        let x = super::dl_rules::ds(&mut st);
        x.root = Some(Box::default());
        // Seed 20 processed clauses with g0 maximal and 20 with g1 maximal -> 400 choices
        // so `rret` reserves `slack > 0` and hits `PRODUCT_POLL` (`go_on` panic) inside fold.
        let ord = Rooted {
            ix: &ix3,
            lits: &run3.lits,
            inv: &inv,
            ground_total: true,
        };
        let mut max = Vec::new();
        for i in 0..20u32 {
            assert!(
                x.store
                    .add(Clause::new(vec![Lit::x(100 + i)], vec![g0]))
                    .is_some()
            );
            assert!(x.store.next(&ix3, Some(&ord), &mut max).is_some());
            assert!(
                x.store
                    .add(Clause::new(vec![Lit::x(200 + i)], vec![g1]))
                    .is_some()
            );
            assert!(x.store.next(&ix3, Some(&ord), &mut max).is_some());
        }
        let before_dl = super::dl_rules::dl(&st).unwrap().bytes() + run3.mtab_bytes();
        let live = Arc::new(AtomicUsize::new(0));
        let mut sink = PanicOnPollSink {
            run: &run3,
            live: live.clone(),
            panic_on_admit_after: None,
            admits: 0,
            panic_on_go_on: true,
        };
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = super::dl_root::rret(&ix3, &mut st, cid, &mut sink);
        }));
        assert!(res.is_err(), "expected simulated go_on panic inside rret");
        let after_dl = super::dl_rules::dl(&st).unwrap().bytes() + run3.mtab_bytes();
        assert_eq!(
            live.load(Ordering::Relaxed),
            after_dl.saturating_sub(before_dl),
            "rret leaked slack or fold reservation on callback unwind"
        );
    }
}

/// Breaking on `out.stopped()` right after `Store::next_commit` in `dl_rules::flush` or
/// `dl_root::flush` must mark `x.store.interrupted = true` so a partial answer never reports
/// the context complete after dropping the popped clause.
#[test]
fn flush_stop_after_next_commit_marks_store_interrupted() {
    struct StopOnCommitSink<'a> {
        run: &'a DlRun,
        stopped: bool,
        arm: bool,
    }
    impl Sink for StopOnCommitSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn admit_dl(&mut self, _: usize) -> bool {
            !self.stopped
        }
        fn commit_dl(&mut self, _: usize, _: usize) {
            if self.arm {
                self.stopped = true;
            }
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    let (ix, _, _) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let run = DlRun::new(false, Default::default());

    for is_root in [false, true] {
        let mut st = State::default();
        let x = super::dl_rules::ds(&mut st);
        // Pre-initialize `x.root` so `index_consts` performs no admission on a non-constant lit.
        x.root = Some(Box::default());
        assert!(x.store.add(Clause::new(vec![], vec![Lit::x(1)])).is_some());
        let mut sink = StopOnCommitSink {
            run: &run,
            stopped: false,
            arm: true,
        };
        let n = if is_root {
            super::dl_root::flush(&ix, &mut st, &mut sink, 16)
        } else {
            super::dl_rules::flush(&ix, 1, &mut st, &mut sink, 16)
        };
        assert_eq!(n, 0);
        let d = super::dl_rules::dl(&st).expect("dl state");
        assert!(
            d.store.interrupted,
            "is_root={is_root}: breaking after next_commit must mark store interrupted"
        );
        assert!(
            !d.is_idle(),
            "is_root={is_root}: interrupted store must not report is_idle() == true"
        );
    }
}

/// Early refusal of `flush_sends` before `dl_root::init` in `Run::process` must clear
/// `ctx.active` back to `false`.
#[test]
fn process_early_root_refusal_clears_ctx_active() {
    let (ix, root, n_nodes) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let ctx: super::saturate::Contexts = (0..n_nodes).map(|_| std::sync::OnceLock::new()).collect();
    let dl = DlRun::new(false, Default::default());
    // `max_memory == base_memory`: adding `CONTEXT_BYTES` for `root` inside `Run::process`
    // fails `flush_sends` right before `dl_root::init`.
    let base = 4096usize;
    let bounds = super::saturate::Bounds {
        threads: 1,
        deadline: None,
        max_memory: Some(base),
        cancelled: None,
        charge: None,
        base_memory: base,
        base_queued: 0,
        peak_memory: base,
        paid_memory: 0,
        dynamic: None,
        dl: Some(&dl),
        partial: false,
        steps: false,
    };
    let (_, stop) = super::saturate::saturate_counted(&ix, &ctx, &[root], &[], &[], &bounds);
    assert!(matches!(stop, Some(super::saturate::Stop::Memory { .. })));
    let root_ctx = ctx[root as usize].get().expect("root context created");
    assert!(
        !root_ctx.is_active(),
        "Run::process early refusal must clear ctx.active"
    );
    assert!(root_ctx.st.lock().interrupted);
}

/// Refusal of `vec_push_admit(&mut x.neg, ...)` inside `dl_rules::init_test` must mark
/// `st.interrupted` and `x.store.interrupted` and stop emitting further conclusions.
#[test]
fn init_test_refusal_marks_interrupted_and_halts() {
    struct AllowFirstOnlySink<'a> {
        run: &'a DlRun,
        allowed: usize,
        me_concls: Vec<Concl>,
    }
    impl Sink for AllowFirstOnlySink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, c: Concl) {
            self.me_concls.push(c);
        }
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            if bytes == 0 {
                return true;
            }
            if self.allowed > 0 {
                self.allowed -= 1;
                true
            } else {
                false
            }
        }
        fn commit_dl(&mut self, _: usize, _: usize) {}
        fn worked(&mut self, _: usize) {}
    }

    let run = DlRun::new(false, Default::default());
    let mut st = State::default();
    let mut sink = AllowFirstOnlySink {
        run: &run,
        allowed: 1, // allows `ds_admit` (Extra + DlSt), refuses `x.neg` Vec allocation
        me_concls: Vec::new(),
    };
    let neg_bit: Ex = 1 << 31;
    let core = [5 | neg_bit, 42];
    super::dl_rules::init_test(&mut st, &core, &mut sink);
    assert!(st.interrupted, "init_test refusal must set st.interrupted");
    let d = super::dl_rules::dl(&st).expect("dl state");
    assert!(
        d.store.interrupted,
        "init_test refusal must set x.store.interrupted"
    );
    assert!(d.neg.is_empty(), "refused neg class must not be pushed");
    assert_eq!(
        sink.me_concls.len(),
        1,
        "init_test must halt and not push subsequent Concl::Sub(42) after refusal"
    );
}

/// `Store::clone()` must recompute `owned_bytes` to match the cloned containers' capacities,
/// and `Store::scan_bytes()` must scan `ByTerm` independently.
#[test]
fn store_clone_preserves_owned_bytes_and_independent_byterm_scan() {
    let mut it = Interner::new();
    let ix = Index::build(&Axioms::default(), &mut it, None, &|| false).unwrap();
    let lits = Lits::default();
    let inv = |r: u32| Some(r ^ 1);
    let mut s = Store::default();
    for i in 0..32u32 {
        let Made::Lit(g1) = lits.lit(XLit::Atom(10 + i, RTerm::C(i % 4)), inv) else {
            panic!()
        };
        let Made::Lit(g2) = lits.lit(XLit::Atom(100 + i, RTerm::C((i + 1) % 4)), inv) else {
            panic!()
        };
        assert!(s.add(Clause::new(vec![g1], vec![g2])).is_some());
    }
    let ord = Rooted {
        ix: &ix,
        lits: &lits,
        inv: &inv,
        ground_total: true,
    };
    let mut max = Vec::new();
    for _ in 0..16 {
        assert!(s.next(&ix, Some(&ord), &mut max).is_some());
    }
    let cloned = s.clone();
    assert_eq!(
        cloned.bytes(),
        cloned.scan_bytes(),
        "Store::clone() must keep owned_bytes == scan_bytes()"
    );
    assert_eq!(
        cloned.terms.as_deref().unwrap().bytes(),
        std::mem::size_of::<super::dl_clause::ByTerm>()
            + super::dl_clause::map_vec_bytes(&cloned.terms.as_deref().unwrap().any)
            + super::dl_clause::map_vec_bytes(&cloned.terms.as_deref().unwrap().eqs),
        "ByTerm::clone() must recompute its owned_bytes"
    );
}

/// Round-2 blocker 1: `Store::next_scratch_bound` must cover the actual counting-allocator peak
/// of `next_prepare_id` (`maximal_rooted` + `Rooted::gt`) for 256 incomparable named class atoms
/// over `x`, both when `max` starts empty (consecutive reallocation overlap) and when `max` is
/// already warm and never grows (comparator must not allocate unreserved heap).
#[test]
fn rooted_maxima_scratch_bound_covers_empty_realloc_overlap_and_warm_comparator() {
    let mut body = String::from("SubClassOf(:A ObjectUnionOf(:B :C))\nClassAssertion(:A :u0)\n");
    for i in 0..256 {
        body.push_str(&format!("SubClassOf(:C{i} :B)\n"));
    }
    let (ix, _, n_nodes) = rooted_index(&body);
    let named: Vec<Lit> = (0..n_nodes as Ex)
        .filter(|&e| super::dl_clause::rank(&ix, e).0 == 2)
        .take(256)
        .map(Lit::x)
        .collect();
    assert_eq!(named.len(), 256, "expected 256 named classes");

    let lits = Lits::default();
    let inv = |_: u32| None;
    let ord = Rooted {
        ix: &ix,
        lits: &lits,
        inv: &inv,
        ground_total: true,
    };
    let mut store = Store::default();
    assert!(store.add(Clause::new(Vec::new(), named)).is_some());
    let idle_run = DlRun::default();
    let id = store.peek_live(&idle_run).expect("queued wide clause");

    // 1. Empty `max`: all 256 incomparable named classes are pushed; final 128 -> 256 growth
    // overlaps old and new buffers under the counting-allocator model.
    let mut empty_max = Vec::new();
    let (views_empty, max_add_empty) = store
        .next_scratch_bound(id, true, &empty_max, empty_max.capacity())
        .expect("scratch bound");
    let scratch_empty = views_empty + max_add_empty;
    let (bound_empty, sample_empty) =
        measure(|| store.next_prepare_id(&ix, Some(&ord), id, &mut empty_max));
    assert!(bound_empty.is_some());
    assert_eq!(
        empty_max.len(),
        256,
        "all 256 named class atoms over x must be mutually incomparable"
    );
    assert!(
        scratch_empty >= sample_empty.peak,
        "empty-max scratch bound ({scratch_empty}) undercounted maximal_rooted peak ({})",
        sample_empty.peak
    );

    // 2. Warm `max` (capacity 256, no growth): views fill their exact allocation and
    // `Rooted::gt` must not allocate heap beyond `views_warm`.
    let mut warm_max = Vec::with_capacity(256);
    let (views_warm, max_add_warm) = store
        .next_scratch_bound(id, true, &warm_max, warm_max.capacity())
        .expect("scratch bound");
    assert_eq!(max_add_warm, 0);
    let scratch_warm = views_warm + max_add_warm;
    let (bound_warm, sample_warm) =
        measure(|| store.next_prepare_id(&ix, Some(&ord), id, &mut warm_max));
    assert!(bound_warm.is_some());
    assert_eq!(warm_max.len(), 256);
    assert!(
        scratch_warm >= sample_warm.peak,
        "warm-max scratch bound ({scratch_warm}) undercounted maximal_rooted peak ({})",
        sample_warm.peak
    );
}

/// Round-2 blocker 2: the `FOLD_PRUNE` (4,096) streaming transition must poll `go_on()`
/// periodically while emitting buffered unions so a stop observable only through `go_on()`
/// (cancellation, deadline, or step-mode work limit while `stopped()` remains `false`) halts
/// promptly instead of emitting all 4,096 buffered unions.
#[test]
fn fold_prune_streaming_transition_polls_go_on_when_stopped_flag_stays_clear() {
    struct TriggerCancelDerive {
        count: Arc<AtomicUsize>,
        min_live: Arc<AtomicUsize>,
        live_ref: Arc<AtomicUsize>,
        cancel_at: usize,
        cancel_flag: Arc<AtomicBool>,
    }
    impl Deriver for TriggerCancelDerive {
        fn derive<S: Sink>(&self, _: &Index, _: &mut State, _: super::dl_rules::Side, _: &mut S) {
            let n = self.count.fetch_add(1, Ordering::Relaxed) + 1;
            let cur = self.live_ref.load(Ordering::Relaxed);
            self.min_live.fetch_min(cur, Ordering::Relaxed);
            if n == self.cancel_at {
                self.cancel_flag.store(true, Ordering::Relaxed);
            }
        }
    }
    struct GoOnOnlySink<'a> {
        run: &'a DlRun,
        live: Arc<AtomicUsize>,
        cancelled: Arc<AtomicBool>,
    }
    impl Sink for GoOnOnlySink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            self.live.fetch_add(bytes, Ordering::Relaxed);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live.fetch_sub(bytes, Ordering::Relaxed);
        }
        fn stopped(&self) -> bool {
            // Intentionally stays false (models unobserved cancellation/deadline or step work).
            false
        }
        fn go_on(&self) -> bool {
            !self.cancelled.load(Ordering::Relaxed)
        }
        fn worked(&mut self, _: usize) {}
    }

    let (ix, _, _) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let run = DlRun::new(false, Default::default());

    let side = |a: Ex| (Vec::new(), vec![Lit::x(a)]);
    let list_a: Vec<_> = (10..75).map(side).collect(); // 65
    let list_b: Vec<_> = (200..265).map(side).collect(); // 65 -> 4,225 > FOLD_PRUNE (4,096)
    let mut found = Found::default();
    found.products(vec![list_a, list_b], (Vec::new(), Vec::new()));

    let count = Arc::new(AtomicUsize::new(0));
    let min_live = Arc::new(AtomicUsize::new(usize::MAX));
    let live_ref = Arc::new(AtomicUsize::new(0));
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let deriver = TriggerCancelDerive {
        count: count.clone(),
        min_live: min_live.clone(),
        live_ref: live_ref.clone(),
        cancel_at: 5,
        cancel_flag: cancel_flag.clone(),
    };
    let mut sink = GoOnOnlySink {
        run: &run,
        live: live_ref.clone(),
        cancelled: cancel_flag.clone(),
    };
    let mut st = State::default();
    super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, &deriver);

    let emitted = count.load(Ordering::Relaxed);
    assert!(
        emitted <= 256,
        "FOLD_PRUNE transition must poll go_on() within one PRODUCT_POLL window (emitted={emitted})"
    );
    assert!(
        min_live.load(Ordering::Relaxed) >= 4096 * std::mem::size_of::<super::dl_rules::Side>(),
        "drained buffer must stay charged during emission (min_live={})",
        min_live.load(Ordering::Relaxed)
    );
    assert_eq!(live_ref.load(Ordering::Relaxed), 0);
    assert!(st.interrupted);
}

/// Round-2 blocker 3: a callback panic inside `rreach` -> `m_registered`'s first sizing scan
/// (`M_SCAN = 1,024` pending registrations) must drop the detached registration vector before
/// crediting its retained capacity back to the ledger on unwind.
#[test]
fn rreach_replay_callback_panic_drops_vec_before_crediting_retained_memory() {
    struct ReplayPanicSink<'a> {
        run: &'a DlRun,
        live: Arc<AtomicUsize>,
        credited: Arc<AtomicUsize>,
        deallocs_at_credit: Arc<AtomicUsize>,
        panic_on_go_on: bool,
    }
    impl Sink for ReplayPanicSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.live.fetch_add(bytes, Ordering::Relaxed);
            true
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live
                .fetch_sub(admitted.saturating_sub(kept), Ordering::Relaxed);
        }
        fn credit_dl(&mut self, bytes: usize) {
            self.deallocs_at_credit
                .store(sample().dealloc_events, Ordering::Relaxed);
            self.live.fetch_sub(bytes, Ordering::Relaxed);
            self.credited.fetch_add(bytes, Ordering::Relaxed);
        }
        fn go_on(&self) -> bool {
            if self.panic_on_go_on {
                panic!("simulated callback panic during m_registered sizing scan");
            }
            true
        }
        fn worked(&mut self, _: usize) {}
    }

    let (ix, root, _) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let mut run = DlRun::new(false, Default::default());
    run.mrepair = MREPAIR_ON;
    let live = Arc::new(AtomicUsize::new(0));
    let credited = Arc::new(AtomicUsize::new(0));
    let deallocs_at_credit = Arc::new(AtomicUsize::new(0));
    let mut st = State::default();

    // Seed 1,024 pending registrations so `m_registered`'s sizing scan hits `Scan::read`'s
    // `go_on()` callback on the 1,024th constant even with no held marker pairs.
    let ctx = 100u32;
    let mut seed_sink = ReplayPanicSink {
        run: &run,
        live: live.clone(),
        credited: credited.clone(),
        deallocs_at_credit: deallocs_at_credit.clone(),
        panic_on_go_on: false,
    };
    for c in 0..1024u32 {
        assert!(super::dl_root::redge(&mut st, ctx, c, &mut seed_sink));
    }

    let mut panic_sink = ReplayPanicSink {
        run: &run,
        live: live.clone(),
        credited: credited.clone(),
        deallocs_at_credit: deallocs_at_credit.clone(),
        panic_on_go_on: true,
    };
    let (res, _) = measure(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = super::dl_root::rreach(&ix, root, &mut st, ctx, &mut panic_sink);
        }))
    });
    assert!(
        res.is_err(),
        "expected simulated go_on panic in m_registered"
    );
    assert!(
        credited.load(Ordering::Relaxed) >= 1024 * std::mem::size_of::<u32>(),
        "rreach unwind must credit freed registration vector (credited={})",
        credited.load(Ordering::Relaxed)
    );
    assert!(
        deallocs_at_credit.load(Ordering::Relaxed) >= 1,
        "detached registration vector must be deallocated before credit_dl runs"
    );
    let expected_live = std::mem::size_of::<super::saturate::Extra>()
        + super::dl_rules::dl(&st).expect("dl state").bytes()
        + run.mtab_bytes();
    assert_eq!(
        live.load(Ordering::Relaxed),
        expected_live,
        "retained ledger must equal Extra + DlSt bytes after rreach unwind"
    );
}

/// Round-2 blocker 4: an early stop or callback panic during single-conclusion delivery in
/// `drain_with_covered` must drop pending product buffers before `held_guard` calls
/// `release_data`.
#[test]
fn drain_single_conclusion_early_stop_and_panic_drop_products_before_release() {
    struct StopOrPanicOnFirst {
        stop_flag: Arc<AtomicBool>,
        should_panic: bool,
    }
    impl Deriver for StopOrPanicOnFirst {
        fn derive<S: Sink>(&self, _: &Index, _: &mut State, _: super::dl_rules::Side, _: &mut S) {
            if self.should_panic {
                // Unwind directly without invoking the global panic hook so
                // `RUST_BACKTRACE=1`/`full` symbolization does not allocate megabytes on
                // the thread-local counting allocator before `held_guard` drops.
                std::panic::resume_unwind(Box::new("simulated panic on first single conclusion"));
            }
            self.stop_flag.store(true, Ordering::Relaxed);
        }
    }
    struct ReleaseInspectSink<'a> {
        run: &'a DlRun,
        live_charge: Arc<AtomicUsize>,
        heap_at_release: Arc<AtomicUsize>,
        watched_deallocs_at_release: Arc<AtomicUsize>,
        released: Arc<AtomicUsize>,
        stop_flag: Arc<AtomicBool>,
    }
    impl Sink for ReleaseInspectSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            self.live_charge.fetch_add(bytes, Ordering::Relaxed);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.heap_at_release.store(sample().live, Ordering::Relaxed);
            self.watched_deallocs_at_release.store(
                watched_deallocations_distinct().unwrap_or(0),
                Ordering::Relaxed,
            );
            self.live_charge.fetch_sub(bytes, Ordering::Relaxed);
            self.released.fetch_add(bytes, Ordering::Relaxed);
        }
        fn stopped(&self) -> bool {
            self.stop_flag.load(Ordering::Relaxed)
        }
        fn worked(&mut self, _: usize) {}
    }

    struct ClearDeallocWatch;
    impl Drop for ClearDeallocWatch {
        fn drop(&mut self) {
            let _ = take_watched_deallocations();
        }
    }

    let (ix, _, _) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let run = DlRun::new(false, Default::default());

    // Measure the exact live heap held by `resume_unwind` itself during stack unwinding
    // so the panic assertion below leaves zero headroom for any surviving product buffer.
    let mut unwind_live = 0usize;
    {
        struct MeasureUnwindLive<'a>(&'a mut usize);
        impl Drop for MeasureUnwindLive<'_> {
            fn drop(&mut self) {
                *self.0 = sample().live;
            }
        }
        let (_, _) = measure(|| {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _probe = MeasureUnwindLive(&mut unwind_live);
                std::panic::resume_unwind(Box::new("simulated panic on first single conclusion"));
            }));
            assert!(res.is_err());
        });
    }
    let single_product_buf_bytes = 256 * std::mem::size_of::<Lit>();
    assert!(
        unwind_live < single_product_buf_bytes,
        "expected baseline unwind payload ({unwind_live} B) to be smaller than one product buffer ({single_product_buf_bytes} B)"
    );

    for should_panic in [false, true] {
        let mut st = State::default();
        let _ = super::dl_rules::ds(&mut st);
        let live_charge = Arc::new(AtomicUsize::new(0));
        let heap_at_release = Arc::new(AtomicUsize::new(usize::MAX));
        let watched_deallocs_at_release = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let deriver = StopOrPanicOnFirst {
            stop_flag: stop_flag.clone(),
            should_panic,
        };
        let mut sink = ReleaseInspectSink {
            run: &run,
            live_charge: live_charge.clone(),
            heap_at_release: heap_at_release.clone(),
            watched_deallocs_at_release: watched_deallocs_at_release.clone(),
            released: released.clone(),
            stop_flag: stop_flag.clone(),
        };

        let mut prods_bytes = 0usize;
        let mut watched_count = 0usize;
        let (_, _) = measure(|| {
            let wide = |base: Ex| (Vec::new(), (base..base + 256).map(Lit::x).collect());
            let list_a: Vec<(Vec<Lit>, Vec<Lit>)> =
                (0..16u32).map(|i| wide(100 + i * 300)).collect();
            let list_b: Vec<_> = (0..4u32).map(|i| wide(10_000 + i * 300)).collect();
            let mut watched = [(0usize, std::alloc::Layout::new::<()>()); 20];
            for (slot, side) in watched.iter_mut().zip(list_a.iter().chain(list_b.iter())) {
                *slot = (
                    side.1.as_ptr().addr(),
                    std::alloc::Layout::array::<Lit>(side.1.capacity()).unwrap(),
                );
            }
            watched_count = watched.len();
            let mut found = Found::default();
            found.push((Vec::new(), vec![Lit::x(1)]));
            found.push((Vec::new(), vec![Lit::x(2)]));
            let one_bytes = found.held_bytes();
            found.products(vec![list_a, list_b], (Vec::new(), Vec::new()));
            prods_bytes = found.held_bytes() - one_bytes;
            watch_deallocations(&watched);
            let _clear_watch = ClearDeallocWatch;
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, &deriver);
            }));
            assert_eq!(res.is_err(), should_panic);
        });

        assert!(
            prods_bytes > 32 << 10,
            "expected wide pending product buffers (prods_bytes={prods_bytes})"
        );
        assert!(
            released.load(Ordering::Relaxed) > 32 << 10,
            "should_panic={should_panic}: expected wide held reservation release"
        );
        assert_eq!(
            watched_deallocs_at_release.load(Ordering::Relaxed),
            watched_count,
            "should_panic={should_panic}: not all {watched_count} pending product buffers were deallocated before held_guard released credit"
        );
        let live_at_release = heap_at_release.load(Ordering::Relaxed);
        let expected_live = if should_panic { unwind_live } else { 0 };
        assert_eq!(
            live_at_release, expected_live,
            "should_panic={should_panic}: live heap at held_guard release ({live_at_release} B) did not match expected baseline ({expected_live} B); pending product buffers ({prods_bytes} B) were still live"
        );
        assert_eq!(live_charge.load(Ordering::Relaxed), 0);
    }
}

/// Phase 1 shared root facts (`SPARKLES_ROOTED_SHARED_FACTS=1`) must preserve exact
/// counting-allocator peak memory coverage under both marker-repair settings.
#[test]
fn shared_facts_end_to_end_peak_memory_covers_counting_allocator_heap() {
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
        let (out, sample) = super::dl_rules::with_shared_facts_override(true, || {
            let mut k = Classifier::of(&rooted_input);
            let limits = Limits {
                threads: 1,
                rooted: true,
                dl_steps: true,
                rooted_marker_repair: repair,
                ..Limits::default()
            };
            measure(|| k.classify(&limits).expect("rooted shared_facts classify"))
        });
        assert!(
            out.stats.peak_memory >= sample.peak,
            "shared_facts repair={repair}: peak_memory ({}) < measured peak ({})",
            out.stats.peak_memory,
            sample.peak
        );
    }
}

/// Sol r1 P1: `dl_root::hyper_at` conjunction (`conj_of`) fan-out must traverse borrowed
/// store clauses directly without allocating an uncharged partner buffer and must pre-admit
/// `Found::one` growth before allocating, under both `shared_facts = false` and `true`.
#[test]
fn hyper_at_conj_admits_peak_and_refuses_without_uncharged_scratch() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    // Declare :H0..:H519 BEFORE :D, :P, :K so P(u0) has a strictly higher Ex ID than every
    // :H_i(u0) and is guaranteed maximal in all 513 partner clauses.
    let mut body = String::new();
    for i in 0..520 {
        body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    body.push_str(
        "SubClassOf(ObjectIntersectionOf(:D :P) :K)\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         ClassAssertion(:Seed :u0)\n",
    );
    let input = axioms(&body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let allowed = super::dl_norm::Allowed {
        rooted: true,
        ..Default::default()
    };
    let mut dl_ax = super::dl_norm::normalize(&outside, &mut it, &mut ax, None, &|| false, allowed)
        .expect("dl_norm")
        .map(|(_, d)| d)
        .unwrap_or_default();
    let root = dl_ax.root.unwrap_or_else(|| {
        let r = it.fresh_class();
        dl_ax.root = Some(r);
        r
    });
    for set in &mut ax.disjoint {
        set.sort_unstable();
    }
    ax.dl = Some(Box::new(dl_ax));
    let d_cls = it.class(&Class::new("urn:tierd1291:D"));
    let p_cls = it.class(&Class::new("urn:tierd1291:P"));
    let h_cls: Vec<Ex> = (0..520)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    let dx = ix.dl.as_deref().expect("rooted dl index");
    let u0 = dx.consts[0].0;
    // `ix.conj_of(D)` yields `(P, conj_cls)` where `conj_cls` is the `D ⊓ P` head literal
    // that `hyper_at` actually emits into `Found::one`.
    let conj_cls = ix
        .conj_of(ix.head(d_cls))
        .iter()
        .find(|&&(p, _)| p == p_cls)
        .map(|&(_, k)| k)
        .expect("conj_of(D) contains P");
    let inv = |r: super::index::Role| dx.inv(&ix, r);
    let mk_atom = |run: &DlRun, cls: Ex| match run.lits.lit(XLit::Atom(cls, RTerm::C(u0)), inv) {
        Made::Lit(l) => l,
        _ => unreachable!(),
    };

    for share in [false, true] {
        let run = DlRun::new(false, Default::default()).with_shared_facts(share);
        let lp = mk_atom(&run, p_cls);
        let ld = mk_atom(&run, d_cls);
        // Intern `lh` BEFORE `lconj` so `lconj` has the highest `Lit` ID and `-> (D ⊓ P)(u0)`
        // subsumes all 513 conclusions with zero `Store` growth in the isolated peak check.
        let lh: Vec<Lit> = h_cls.iter().map(|&c| mk_atom(&run, c)).collect();
        let lconj = mk_atom(&run, conj_cls);
        // Warm `run.table`'s first shared segment outside the measured per-context flush so
        // `pred_id` in non-root `process` does not charge a one-time 32 KiB table segment
        // against `hyper_at`'s transient peak.
        let _ = run.table.push(Clause::new(Vec::new(), vec![ld]));

        let build_state = |subsume_k: bool| {
            let mut st = State::default();
            let mut setup_sink = BudgetSink {
                run: &run,
                cap: usize::MAX,
                live: 0,
                peak: 0,
                stopped: false,
            };
            let x = super::dl_rules::ds(&mut st);
            let ground_total = super::dl_clause::ground_total();
            let o = Rooted {
                ix: &ix,
                lits: &run.lits,
                inv: &inv,
                ground_total,
            };
            let mut max = Vec::new();
            // Insert 513 live unselected partner clauses for P(u0), each with 8 head lits,
            // and process them so `store.by_xl[lp]` holds all 513 partners.
            for i in 0..513usize {
                let mut head = vec![lp];
                for j in 0..7usize {
                    head.push(lh[(i + j * 73) % 520]);
                }
                let c = Clause::new(vec![Lit::x(h_cls[i])], head);
                let _ = x.store.add(c);
                let id = x.store.peek_live(&run).expect("partner queued");
                let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
                let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
                assert!(
                    max.contains(&lp),
                    "partner {i} must index lp as maximal literal"
                );
            }
            if subsume_k {
                // Pre-install `-> (D ⊓ P)(u0)` so draining the 513 conclusions adds 0 bytes
                // to Store, isolating `hyper_at` + `Found::one` peak memory from Store growth.
                let _ = x.store.add(Clause::new(Vec::new(), vec![lconj]));
                let id = x.store.peek_live(&run).expect("conj queued");
                let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
                let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
            }
            // Queue the trigger clause `-> D(u0)` (which has 513 conj_of partners).
            let _ = x.store.add(Clause::new(Vec::new(), vec![ld]));
            // Mark `sent` on the trigger clause so non-root `pred_id` does not clone into
            // the shared `run.table` during the isolated `hyper_at` peak check.
            if let Some(last) = x.store.clauses.last_mut() {
                last.sent = Some(0);
            }
            // Ensure `x.root` is already allocated outside the measured flush.
            super::dl_root::register_max(&ix, root, &mut st, &[], &mut setup_sink);
            st
        };

        // 1. Unbounded counting-allocator peak coverage test (both root and non-root contexts),
        //    with `subsume_k = true` so Store adds 0 bytes and `sink.peak` must cover `hyper_at`'s
        //    own transient heap directly (not masked by Store growth).
        for is_root in [true, false] {
            let mut st = build_state(true);
            let mut sink = BudgetSink {
                run: &run,
                cap: usize::MAX,
                live: 0,
                peak: 0,
                stopped: false,
            };
            let (processed, sample) = measure(|| {
                if is_root {
                    super::dl_root::flush(&ix, &mut st, &mut sink, 1)
                } else {
                    super::dl_rules::flush(&ix, d_cls, &mut st, &mut sink, 1)
                }
            });
            assert_eq!(
                processed, 1,
                "share={share}, is_root={is_root}: expected 1 processed clause"
            );
            assert!(
                sink.peak >= sample.peak,
                "share={share}, is_root={is_root}: hyper_at conj peak charge ({}) undercounted measured peak heap ({})",
                sink.peak,
                sample.peak
            );
            // All temporary Found/max_guard reservations must be released after flush.
            let retained_after = sink.live;
            assert!(
                retained_after <= 4096,
                "share={share}, is_root={is_root}: leaked temporary reservation ({retained_after} B)"
            );
        }

        // 2. Tight-cap refusal test (both root and non-root): budget allows `next` scratch
        //    (<= 4 KiB) but is far below 513 * (~128 B) = ~64 KiB of conclusions; refusal must
        //    happen BEFORE allocating the 513 conclusions or any partner buffer.
        for is_root in [true, false] {
            let mut st_tight = build_state(false);
            let cap = 4 << 10;
            let mut tight = BudgetSink {
                run: &run,
                cap,
                live: 0,
                peak: 0,
                stopped: false,
            };
            let (_, tight_sample) = measure(|| {
                if is_root {
                    super::dl_root::flush(&ix, &mut st_tight, &mut tight, 1)
                } else {
                    super::dl_rules::flush(&ix, d_cls, &mut st_tight, &mut tight, 1)
                }
            });
            assert!(
                tight.stopped,
                "share={share}, is_root={is_root}: tight cap must refuse 513-partner hyper_at conj fan-out"
            );
            assert!(
                super::dl_rules::dl(&st_tight).is_some_and(|x| x.store.interrupted),
                "share={share}, is_root={is_root}: refusal must mark store interrupted"
            );
            assert!(
                tight_sample.peak <= cap + 4096,
                "share={share}, is_root={is_root}: tight cap refusal overshot cap ({cap}): measured peak = {}",
                tight_sample.peak
            );
        }
    }
}

/// Sol r2 P1: after `push_partners_admitted` fills `Found::one` with 513 conjunction
/// conclusions (exact initial capacity 513), a subsequent producer on the same `Found` —
/// `hyper_roles` via a universal `D ⊑ ∀r.Q` over a stored role clause `r(u0, u1)` — must
/// pre-admit both the `Found::one` reallocation overlap and the new `Side` BEFORE allocating.
/// Under a tight cap that grants the 513-partner conjunction batch (~68 KiB) and refuses
/// `hyper_roles`'s next growth (513 -> 1026 realloc overlap ~65 KiB), refusal must occur
/// without exceeding the cap in both `shared_facts = false` and `true`, root and non-root.
#[test]
fn hyper_at_conj_then_hyper_roles_admits_peak_and_refuses_tight_cap() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    // Declare :H0..:H519 BEFORE :D, :P, :K, :Q so P(u0) is maximal in all 513 partner clauses.
    // Give :D BOTH a conjunction rule `D ⊓ P ⊑ K` AND a universal `D ⊑ ∀r.Q` (which normalizes
    // so `ix.head(D)` or a told subsumer? Direct `ObjectAllValuesFrom(:r :Q)` is a Forall node,
    // so use `D = ObjectAllValuesFrom(:r :Q)` via `EquivalentClasses` or `SubClassOf(D ⊓ P, K)`
    // where `D` itself is `ObjectAllValuesFrom(:r :Q)`, OR `SubClassOf(ObjectSomeValuesFrom(:r_inv :D) :Q)`
    // so `alls(ix, D)` directly yields `(r, Q)` on `D` itself!).
    let mut body = String::new();
    for i in 0..520 {
        body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    body.push_str(
        "Declaration(ObjectProperty(:r))\n\
         Declaration(Class(:P))\n\
         Declaration(Class(:Q))\n\
         Declaration(Class(:K))\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         ClassAssertion(:Seed :u0)\n\
         ClassAssertion(:Seed :u1)\n",
    );
    let input = axioms(&body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let allowed = super::dl_norm::Allowed {
        rooted: true,
        ..Default::default()
    };
    let mut dl_ax = super::dl_norm::normalize(&outside, &mut it, &mut ax, None, &|| false, allowed)
        .expect("dl_norm")
        .map(|(_, d)| d)
        .unwrap_or_default();
    let root = dl_ax.root.unwrap_or_else(|| {
        let r = it.fresh_class();
        dl_ax.root = Some(r);
        r
    });
    for set in &mut ax.disjoint {
        set.sort_unstable();
    }
    ax.dl = Some(Box::new(dl_ax));
    let p_cls = it.class(&Class::new("urn:tierd1291:P"));
    let q_cls = it.class(&Class::new("urn:tierd1291:Q"));
    let k_cls = it.class(&Class::new("urn:tierd1291:K"));
    let r_role = it.role(super::index::RoleName::Object(
        crate::model::ObjectProperty::new("urn:tierd1291:r"),
    ));
    let d_cls = it.node(super::index::Node::Forall(r_role, q_cls));
    let dp_conj = it.node(super::index::Node::Conj(d_cls, p_cls));
    ax.subs.push((dp_conj, k_cls));
    let h_cls: Vec<Ex> = (0..520)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    let dx = ix.dl.as_deref().expect("rooted dl index");
    let u0 = dx.consts[0].0;
    let u1 = dx.consts[1].0;
    let conj_cls = ix
        .conj_of(ix.head(d_cls))
        .iter()
        .find(|&&(p, _)| p == p_cls)
        .map(|&(_, k)| k)
        .expect("conj_of(D) contains P");
    let mut d_alls = Vec::new();
    super::dl_rules::alls(&ix, d_cls, &mut d_alls);
    assert!(
        d_alls.iter().any(|&(_, c)| c == q_cls),
        "D must carry universal restriction yielding Q for hyper_roles"
    );
    let inv = |r: super::index::Role| dx.inv(&ix, r);
    let mk_atom =
        |run: &DlRun, cls: Ex, u: u32| match run.lits.lit(XLit::Atom(cls, RTerm::C(u)), inv) {
            Made::Lit(l) => l,
            _ => unreachable!(),
        };
    let mk_role = |run: &DlRun| match run
        .lits
        .lit(XLit::Role(r_role, RTerm::C(u0), RTerm::C(u1)), inv)
    {
        Made::Lit(l) => l,
        _ => unreachable!(),
    };

    for share in [false, true] {
        let run = DlRun::new(false, Default::default()).with_shared_facts(share);
        let lp = mk_atom(&run, p_cls, u0);
        let ld = mk_atom(&run, d_cls, u0);
        let lrole = mk_role(&run);
        let lh: Vec<Lit> = h_cls.iter().map(|&c| mk_atom(&run, c, u0)).collect();
        let lconj = mk_atom(&run, conj_cls, u0);
        let lq = mk_atom(&run, q_cls, u1);
        let _ = run.table.push(Clause::new(Vec::new(), vec![ld]));

        let build_state = |subsume_outputs: bool| {
            let mut st = State::default();
            let mut setup_sink = BudgetSink {
                run: &run,
                cap: usize::MAX,
                live: 0,
                peak: 0,
                stopped: false,
            };
            let x = super::dl_rules::ds(&mut st);
            let ground_total = super::dl_clause::ground_total();
            let o = Rooted {
                ix: &ix,
                lits: &run.lits,
                inv: &inv,
                ground_total,
            };
            let mut max = Vec::new();
            // 1. Insert 513 live unselected partner clauses for P(u0).
            for i in 0..513usize {
                let mut head = vec![lp];
                for j in 0..7usize {
                    head.push(lh[(i + j * 73) % 520]);
                }
                let c = Clause::new(vec![Lit::x(h_cls[i])], head);
                let _ = x.store.add(c);
                let id = x.store.peek_live(&run).expect("partner queued");
                let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
                let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
                assert!(max.contains(&lp));
            }
            // 2. Insert a live unselected role clause `H0(x) -> r(u0, u1)` indexed in `roles_of[u0]`
            //    so `hyper_roles` fires right AFTER the 513-partner `conj_of` batch on the same `Found`.
            let role_clause = Clause::new(vec![Lit::x(h_cls[0])], vec![lrole]);
            let _ = x.store.add(role_clause);
            let rid = x.store.peek_live(&run).expect("role clause queued");
            let _ = x.store.next_prepare_id(&ix, Some(&o), rid, &mut max);
            let _ = x.store.next_commit(&ix, Some(&o), rid, &max, &run);
            assert!(max.contains(&lrole), "role literal must be maximal");
            super::dl_root::index_consts(&ix, x, &max, rid, &mut setup_sink);
            if subsume_outputs {
                for unit_lit in [lconj, lq] {
                    let _ = x.store.add(Clause::new(Vec::new(), vec![unit_lit]));
                    let id = x.store.peek_live(&run).expect("subsumer queued");
                    let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
                    let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
                }
            }
            // 3. Queue trigger clause `-> D(u0)`.
            let _ = x.store.add(Clause::new(Vec::new(), vec![ld]));
            if let Some(last) = x.store.clauses.last_mut() {
                last.sent = Some(0);
            }
            super::dl_root::register_max(&ix, root, &mut st, &[], &mut setup_sink);
            st
        };

        // 1. Unbounded counting-allocator peak coverage: `sink.peak` must cover BOTH the
        //    513-partner conjunction batch AND `hyper_roles`'s 513 -> 1026 vector growth.
        for is_root in [true, false] {
            let mut st = build_state(true);
            let mut sink = BudgetSink {
                run: &run,
                cap: usize::MAX,
                live: 0,
                peak: 0,
                stopped: false,
            };
            let (processed, sample) = measure(|| {
                if is_root {
                    super::dl_root::flush(&ix, &mut st, &mut sink, 1)
                } else {
                    super::dl_rules::flush(&ix, d_cls, &mut st, &mut sink, 1)
                }
            });
            assert_eq!(processed, 1);
            assert!(
                sink.peak >= sample.peak,
                "share={share}, is_root={is_root}: mixed conj+hyper_roles peak charge ({}) < measured peak ({})",
                sink.peak,
                sample.peak
            );
            assert!(
                sink.live <= 4096,
                "share={share}, is_root={is_root}: leaked reservation ({} B)",
                sink.live
            );
        }

        // 2. Tight cap that GRANTS the 513-partner conjunction batch (~70 KiB) but REFUSES
        //    `hyper_roles`'s subsequent 513 -> 1026 reallocation (~65 KiB overlap -> ~135 KiB):
        //    must stop cleanly BEFORE reallocating `Found::one` or constructing the new `Side`!
        let conj_batch_bytes = 513
            * (std::mem::size_of::<(super::dl_rules::Side, &'static str)>()
                + 9 * std::mem::size_of::<Lit>());
        let mid_cap = conj_batch_bytes + (16 << 10);
        for is_root in [true, false] {
            let mut st_mid = build_state(false);
            let mut tight = BudgetSink {
                run: &run,
                cap: mid_cap,
                live: 0,
                peak: 0,
                stopped: false,
            };
            let (_, sample) = measure(|| {
                if is_root {
                    super::dl_root::flush(&ix, &mut st_mid, &mut tight, 1)
                } else {
                    super::dl_rules::flush(&ix, d_cls, &mut st_mid, &mut tight, 1)
                }
            });
            assert!(
                tight.stopped,
                "share={share}, is_root={is_root}: mid cap ({mid_cap}) must refuse hyper_roles growth after 513-partner conj batch"
            );
            assert!(
                super::dl_rules::dl(&st_mid).is_some_and(|x| x.store.interrupted),
                "share={share}, is_root={is_root}: hyper_roles refusal must mark store interrupted"
            );
            assert!(
                sample.peak <= mid_cap + 4096,
                "share={share}, is_root={is_root}: hyper_roles growth overshot mid cap ({mid_cap}): measured peak = {}",
                sample.peak
            );
            assert!(
                tight.peak >= sample.peak,
                "share={share}, is_root={is_root}: admitted peak ({}) < measured peak ({}) under mid cap",
                tight.peak,
                sample.peak
            );
        }
    }
}

/// Sol r2 Other checks #2: many small conjunction batches (256 distinct conjunction rules
/// with 1 partner each) must grow `Found::one` geometrically with O(1) `held_bytes()` sizing,
/// covering peak heap and refusing cleanly under a tight cap.
#[test]
fn hyper_at_many_small_conj_batches_admits_peak_and_refuses_tight_cap() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    const N: usize = 256;
    let mut body = String::new();
    for i in 0..N {
        body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    for i in 0..N {
        body.push_str(&format!(
            "SubClassOf(ObjectIntersectionOf(:D :P{i}) :K{i})\n"
        ));
    }
    body.push_str(
        "SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         ClassAssertion(:Seed :u0)\n",
    );
    let input = axioms(&body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let allowed = super::dl_norm::Allowed {
        rooted: true,
        ..Default::default()
    };
    let mut dl_ax = super::dl_norm::normalize(&outside, &mut it, &mut ax, None, &|| false, allowed)
        .expect("dl_norm")
        .map(|(_, d)| d)
        .unwrap_or_default();
    let root = dl_ax.root.unwrap_or_else(|| {
        let r = it.fresh_class();
        dl_ax.root = Some(r);
        r
    });
    for set in &mut ax.disjoint {
        set.sort_unstable();
    }
    ax.dl = Some(Box::new(dl_ax));
    let d_cls = it.class(&Class::new("urn:tierd1291:D"));
    let p_cls: Vec<Ex> = (0..N)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:P{i}"))))
        .collect();
    let h_cls: Vec<Ex> = (0..N)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    let dx = ix.dl.as_deref().expect("rooted dl index");
    let u0 = dx.consts[0].0;
    let inv = |r: super::index::Role| dx.inv(&ix, r);
    let mk_atom = |run: &DlRun, cls: Ex| match run.lits.lit(XLit::Atom(cls, RTerm::C(u0)), inv) {
        Made::Lit(l) => l,
        _ => unreachable!(),
    };
    let conj_classes: Vec<Ex> = ix.conj_of(ix.head(d_cls)).iter().map(|&(_, k)| k).collect();
    assert_eq!(conj_classes.len(), N);

    let run = DlRun::new(false, Default::default()).with_shared_facts(true);
    let ld = mk_atom(&run, d_cls);
    let lp: Vec<Lit> = p_cls.iter().map(|&c| mk_atom(&run, c)).collect();
    let lh: Vec<Lit> = h_cls.iter().map(|&c| mk_atom(&run, c)).collect();
    let lconjs: Vec<Lit> = conj_classes.iter().map(|&c| mk_atom(&run, c)).collect();
    let _ = run.table.push(Clause::new(Vec::new(), vec![ld]));

    let build_state = |subsume: bool| {
        let mut st = State::default();
        let mut setup_sink = BudgetSink {
            run: &run,
            cap: usize::MAX,
            live: 0,
            peak: 0,
            stopped: false,
        };
        let x = super::dl_rules::ds(&mut st);
        let ground_total = super::dl_clause::ground_total();
        let o = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total,
        };
        let mut max = Vec::new();
        for i in 0..N {
            let c = Clause::new(vec![Lit::x(h_cls[i])], vec![lp[i], lh[i]]);
            let _ = x.store.add(c);
            let id = x.store.peek_live(&run).expect("partner queued");
            let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
            let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
            assert!(max.contains(&lp[i]));
        }
        if subsume {
            for &lc in &lconjs {
                let _ = x.store.add(Clause::new(Vec::new(), vec![lc]));
                let id = x.store.peek_live(&run).expect("subsumer queued");
                let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
                let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
            }
        }
        let _ = x.store.add(Clause::new(Vec::new(), vec![ld]));
        if let Some(last) = x.store.clauses.last_mut() {
            last.sent = Some(0);
        }
        super::dl_root::register_max(&ix, root, &mut st, &[], &mut setup_sink);
        st
    };

    // Unbounded peak check
    let mut st = build_state(true);
    let mut sink = BudgetSink {
        run: &run,
        cap: usize::MAX,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (processed, sample) = measure(|| super::dl_root::flush(&ix, &mut st, &mut sink, 1));
    assert_eq!(processed, 1);
    assert!(
        sink.peak >= sample.peak,
        "many small conj batches: peak charge ({}) < measured peak ({})",
        sink.peak,
        sample.peak
    );
    assert!(sink.live <= 4096);

    // Tight cap check (refuses mid-way through geometric growth without overshooting cap)
    let mut st_tight = build_state(false);
    let cap = 8 << 10;
    let mut tight = BudgetSink {
        run: &run,
        cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, tight_sample) = measure(|| super::dl_root::flush(&ix, &mut st_tight, &mut tight, 1));
    assert!(
        tight.stopped,
        "tight cap must refuse 256 small conj batches"
    );
    assert!(super::dl_rules::dl(&st_tight).is_some_and(|x| x.store.interrupted));
    assert!(
        tight_sample.peak <= cap + 4096,
        "many small conj batches tight cap overshot ({cap}): measured peak = {}",
        tight_sample.peak
    );
}

/// Sol r3 P1: unit-triggered disjointness in `dl_rules::sub` must walk borrowed partners
/// without pre-cloning `Vec<Side>`, stop before allocating when a tight cap refuses the
/// very first output, and construct exact-capacity residual heads (`capacity() == len()`).
#[test]
fn sub_disjointness_admits_before_alloc_and_charges_exact_residual_capacity() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    let mut body = String::new();
    for i in 0..520 {
        body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    body.push_str(
        "Declaration(Class(:D))\n\
         Declaration(Class(:M))\n\
         DisjointClasses(:D :M)\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n",
    );
    let input = axioms(&body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let dl_ax = super::dl_norm::normalize(
        &outside,
        &mut it,
        &mut ax,
        None,
        &|| false,
        super::dl_norm::Allowed::default(),
    )
    .expect("dl_norm")
    .map(|(_, d)| d)
    .unwrap_or_default();
    for set in &mut ax.disjoint {
        set.sort_unstable();
    }
    ax.dl = Some(Box::new(dl_ax));
    let d_cls = it.class(&Class::new("urn:tierd1291:D"));
    let m_cls = it.class(&Class::new("urn:tierd1291:M"));
    let h_cls: Vec<Ex> = (0..520)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();

    // First verify `side` produces an exact-capacity residual head for a 5-literal residual
    // (filtered `.collect()` previously allocated capacity 8 for length 5).
    let sample_clause = Clause::new(
        vec![Lit::x(h_cls[0])],
        vec![
            Lit::x(m_cls),
            Lit::x(h_cls[1]),
            Lit::x(h_cls[2]),
            Lit::x(h_cls[3]),
            Lit::x(h_cls[4]),
            Lit::x(h_cls[5]),
        ],
    );
    let residual = super::dl_rules::side(&sample_clause, Lit::x(m_cls));
    assert_eq!(residual.1.len(), 5);
    assert_eq!(
        residual.1.capacity(),
        residual.1.len(),
        "side() residual head must have exact capacity (no over-allocation from filtered collect)"
    );

    let run = DlRun::new(false, Default::default());
    let build_state = |subsume_outputs: bool| {
        let mut st = State::default();
        let x = super::dl_rules::ds(&mut st);
        let mut max = Vec::new();
        for i in 0..513usize {
            // 1 body literal + 5 residual head literals beside maximal M(x).
            let mut head = vec![Lit::x(m_cls)];
            for j in 0..5usize {
                head.push(Lit::x(h_cls[(i + j * 73) % 520]));
            }
            let c = Clause::new(vec![Lit::x(h_cls[i])], head);
            let _ = x.store.add(c);
            let id = x.store.peek_live(&run).expect("partner queued");
            let _ = x.store.next_prepare_id(&ix, None, id, &mut max);
            let _ = x.store.next_commit(&ix, None, id, &max, &run);
            assert!(max.contains(&Lit::x(m_cls)), "M(x) must be maximal");
        }
        if subsume_outputs {
            for &hc in &h_cls {
                let _ = x.store.add(Clause::new(Vec::new(), vec![Lit::x(hc)]));
                let id = x.store.peek_live(&run).expect("subsumer queued");
                let _ = x.store.next_prepare_id(&ix, None, id, &mut max);
                let _ = x.store.next_commit(&ix, None, id, &max, &run);
            }
        }
        st
    };

    // 1. Tight cap that refuses the very first output: must stop before allocating partners
    //    or growing `Found::one`.
    let cap = 32usize;
    let mut st_tight = build_state(false);
    let mut tight = BudgetSink {
        run: &run,
        cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, tight_sample) = measure(|| {
        super::dl_rules::sub(&ix, d_cls, &mut st_tight, d_cls, &mut tight);
    });
    assert!(
        tight.stopped,
        "cap ({cap}) must refuse first disjointness conclusion"
    );
    assert!(
        super::dl_rules::dl(&st_tight).is_some_and(|x| x.store.interrupted),
        "disjointness refusal must mark store interrupted"
    );
    assert!(
        tight_sample.peak <= cap + 2048,
        "disjointness allocated before first admission: measured peak = {}, cap = {cap}",
        tight_sample.peak
    );

    // 2. Unbounded run with nonempty residual heads: admitted peak must cover actual peak
    //    heap and release cleanly without leaks.
    let mut st_full = build_state(true);
    let mut full = BudgetSink {
        run: &run,
        cap: usize::MAX,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, full_sample) = measure(|| {
        super::dl_rules::sub(&ix, d_cls, &mut st_full, d_cls, &mut full);
    });
    assert!(!full.stopped);
    assert!(
        full.peak >= full_sample.peak,
        "sub disjointness peak charge ({}) < measured peak ({})",
        full.peak,
        full_sample.peak
    );
    assert!(
        full.live <= 4096,
        "sub disjointness leaked reservation ({} B)",
        full.live
    );
}

/// Sol r3 P2: `trace_shared_parts` must format borrowed literal iterators directly into
/// the trace writer with zero heap allocations (no combined `String`s, no per-literal `tr`
/// allocations, no `tr_ctx` `String`).
#[test]
fn trace_shared_parts_zero_alloc_on_suppressed_batch() {
    let body = "Declaration(Class(:D))\n\
                Declaration(Class(:P))\n\
                Declaration(Class(:K))\n\
                Declaration(Class(:H0))\n\
                Declaration(Class(:H1))\n\
                SubClassOf(ObjectIntersectionOf(:D :P) :K)\n\
                SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
                ClassAssertion(:Seed :u0)\n";
    let input = axioms(body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let dl_ax = super::dl_norm::normalize(
        &outside,
        &mut it,
        &mut ax,
        None,
        &|| false,
        super::dl_norm::Allowed {
            rooted: true,
            ..Default::default()
        },
    )
    .expect("dl_norm")
    .map(|(_, d)| d)
    .unwrap_or_default();
    ax.dl = Some(Box::new(dl_ax));
    let p_cls = it.class(&Class::new("urn:tierd1291:P"));
    let k_cls = it.class(&Class::new("urn:tierd1291:K"));
    let h0_cls = it.class(&Class::new("urn:tierd1291:H0"));
    let h1_cls = it.class(&Class::new("urn:tierd1291:H1"));
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    let dx = ix.dl.as_deref().expect("rooted dl index");
    let u0 = dx.consts[0].0;
    let inv = |r: super::index::Role| dx.inv(&ix, r);
    let run = DlRun::new(false, Default::default()).with_shared_facts(true);
    let mk_atom = |cls: Ex| match run.lits.lit(XLit::Atom(cls, RTerm::C(u0)), inv) {
        Made::Lit(l) => l,
        _ => unreachable!(),
    };
    let lp = mk_atom(p_cls);
    let lk = mk_atom(k_cls);
    let lh0 = mk_atom(h0_cls);
    let lh1 = mk_atom(h1_cls);
    let st = State::default();
    let prem_body = [lh0];
    let part_body = [lh1];
    let prem_head = [lh0];
    let part_head = [lp, lh1];

    // 1. Format parity check against `tr_ctx` / `tr`.
    let mut rendered = Vec::new();
    super::dl_root::write_shared_parts(
        &mut rendered,
        &ix,
        &st,
        &run.lits,
        "hyperc",
        &prem_body,
        &part_body,
        &prem_head,
        &part_head,
        Some(lp),
        Some(lk),
    )
    .expect("write_shared_parts");
    let expected = format!(
        "TR S {} hyperc {} -> {}\n",
        super::dl_rules::tr_ctx(&st, false),
        super::dl_rules::tr(&ix, &run.lits, &[lh0, lh1]),
        super::dl_rules::tr(&ix, &run.lits, &[lh0, lh1, lk]),
    );
    assert_eq!(String::from_utf8(rendered).unwrap(), expected);

    // 2. Direct zero-allocation check on `trace_shared_parts`.
    let _trace = super::dl_rules::force_test_trace();
    let (_, trace_sample) = measure(|| {
        for _ in 0..32 {
            super::dl_root::trace_shared_parts(
                &ix,
                &st,
                &run.lits,
                "hyperc",
                &prem_body,
                &part_body,
                &prem_head,
                &part_head,
                Some(lp),
                Some(lk),
            );
        }
    });
    assert_eq!(
        trace_sample.peak, 0,
        "trace_shared_parts must not allocate heap scratch (peak = {} B)",
        trace_sample.peak
    );

    // 3. Suppressed-batch count pass in `push_partners_admitted` with `tracing()` enabled:
    //    every partner `-> P(u0)` is a certified root fact and the merged conclusion
    //    `-> K(u0)` is a D1-routable ground unit, so all 64 outputs are suppressed in the
    //    count pass with zero heap allocations.
    struct NoopSink<'a>(&'a DlRun);
    impl Sink for NoopSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.0
        }
    }
    let mut st_sup = State::default();
    let mut setup_sink = NoopSink(&run);
    {
        let x = super::dl_rules::ds(&mut st_sup);
        x.given_fact = true;
        let ground_total = super::dl_clause::ground_total();
        let o = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total,
        };
        let mut max = Vec::new();
        let _ = x.store.add(Clause::new(Vec::new(), vec![lp]));
        let id = x.store.peek_live(&run).expect("P(u0) fact queued");
        let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
        let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
        x.store.clauses[id as usize].fact = true;
        // Duplicate the live index entry 63 more times so the suppressed count pass walks
        // 64 certified fact partners in a single `push_partners_admitted` call.
        for _ in 0..63 {
            x.store.by_xl.entry(lp).or_default().push(id);
        }
    }
    super::dl_root::register_max(&ix, p_cls, &mut st_sup, &[lp], &mut setup_sink);
    let prem = super::dl_rules::unit();
    let mut found = Found::default();
    found.rule = "hyperc";
    let mut guard = super::saturate::Guard::new(&mut setup_sink);
    let (ok, count_pass_sample) = measure(|| {
        super::dl_root::push_partners_admitted(
            &ix,
            &mut st_sup,
            &mut guard,
            p_cls,
            u0,
            Some(k_cls),
            true,
            &prem,
            &mut found,
        )
    });
    assert!(ok);
    assert_eq!(found.held_bytes(), 0, "all 64 outputs must be suppressed");
    assert_eq!(
        count_pass_sample.peak, 0,
        "suppressed-batch count pass with tracing enabled must allocate 0 bytes (peak = {} B)",
        count_pass_sample.peak
    );
}

/// Sol r3 requested-checks note: general ground Hyper in `dl_root::hyper_at` must admit
/// `Found::prods` growth and product list/base payloads before allocating `lists` or
/// appending to `Found::prods`, including right after an admitted conjunction batch.
#[test]
fn hyper_at_conj_then_ground_hyper_product_admits_peak_and_refuses_tight_cap() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    let mut body = String::new();
    for i in 0..520 {
        body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    body.push_str(
        "Declaration(Class(:D))\n\
         Declaration(Class(:P))\n\
         Declaration(Class(:Q))\n\
         Declaration(Class(:K))\n\
         Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         SubClassOf(ObjectIntersectionOf(:D :P) :K)\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         ClassAssertion(:Seed :u0)\n",
    );
    let input = axioms(&body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let allowed = super::dl_norm::Allowed {
        rooted: true,
        ..Default::default()
    };
    let mut dl_ax = super::dl_norm::normalize(&outside, &mut it, &mut ax, None, &|| false, allowed)
        .expect("dl_norm")
        .map(|(_, d)| d)
        .unwrap_or_default();
    let root = dl_ax.root.unwrap_or_else(|| {
        let r = it.fresh_class();
        dl_ax.root = Some(r);
        r
    });
    for set in &mut ax.disjoint {
        set.sort_unstable();
    }
    let d_cls = it.class(&Class::new("urn:tierd1291:D"));
    let p_cls = it.class(&Class::new("urn:tierd1291:P"));
    let q_cls = it.class(&Class::new("urn:tierd1291:Q"));
    let a_cls = it.class(&Class::new("urn:tierd1291:A"));
    let b_cls = it.class(&Class::new("urn:tierd1291:B"));
    let h_cls: Vec<Ex> = (0..520)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    // Direct disjunctive case-analysis clause `D(x) ∧ Q(x) -> A(x) ∨ B(x)` so `hyper_at`
    // queues a general ground Hyper product right after the 513-partner `D ⊓ P ⊑ K` batch.
    dl_ax.clauses.push((vec![d_cls, q_cls], vec![a_cls, b_cls]));
    ax.dl = Some(Box::new(dl_ax));
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    let dx = ix.dl.as_deref().expect("rooted dl index");
    let u0 = dx.consts[0].0;
    let conj_cls = ix
        .conj_of(ix.head(d_cls))
        .iter()
        .find(|&&(p, _)| p == p_cls)
        .map(|&(_, k)| k)
        .expect("conj_of(D) contains P");
    assert!(
        dx.by_body.get(&d_cls).is_some_and(|ids| !ids.is_empty()),
        "D must trigger general ground Hyper clause"
    );
    let inv = |r: super::index::Role| dx.inv(&ix, r);
    let mk_atom = |run: &DlRun, cls: Ex| match run.lits.lit(XLit::Atom(cls, RTerm::C(u0)), inv) {
        Made::Lit(l) => l,
        _ => unreachable!(),
    };

    for share in [false, true] {
        let run = DlRun::new(false, Default::default()).with_shared_facts(share);
        let ld = mk_atom(&run, d_cls);
        let lp = mk_atom(&run, p_cls);
        let lq = mk_atom(&run, q_cls);
        let la = mk_atom(&run, a_cls);
        let lb = mk_atom(&run, b_cls);
        let lconj = mk_atom(&run, conj_cls);
        let lh: Vec<Lit> = h_cls.iter().map(|&c| mk_atom(&run, c)).collect();
        let _ = run.table.push(Clause::new(Vec::new(), vec![ld]));

        let build_state = |subsume_outputs: bool| {
            let mut st = State::default();
            let mut setup_sink = BudgetSink {
                run: &run,
                cap: usize::MAX,
                live: 0,
                peak: 0,
                stopped: false,
            };
            let x = super::dl_rules::ds(&mut st);
            let ground_total = super::dl_clause::ground_total();
            let o = Rooted {
                ix: &ix,
                lits: &run.lits,
                inv: &inv,
                ground_total,
            };
            let mut max = Vec::new();
            // 1. 513 live unselected partner clauses for P(u0) (conjunction batch -> Found::one).
            for i in 0..513usize {
                let mut head = vec![lp];
                for j in 0..7usize {
                    head.push(lh[(i + j * 73) % 520]);
                }
                let c = Clause::new(vec![Lit::x(h_cls[i])], head);
                let _ = x.store.add(c);
                let id = x.store.peek_live(&run).expect("P partner queued");
                let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
                let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
                assert!(max.contains(&lp));
            }
            // 2. 513 live unselected partner clauses for Q(u0) (general ground Hyper -> Found::prods).
            for i in 0..513usize {
                let mut head = vec![lq];
                for j in 0..7usize {
                    head.push(lh[(i + j * 67) % 520]);
                }
                let c = Clause::new(vec![Lit::x(h_cls[i])], head);
                let _ = x.store.add(c);
                let id = x.store.peek_live(&run).expect("Q partner queued");
                let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
                let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
                assert!(max.contains(&lq));
            }
            if subsume_outputs {
                for unit_lit in [lconj, la, lb] {
                    let _ = x.store.add(Clause::new(Vec::new(), vec![unit_lit]));
                    let id = x.store.peek_live(&run).expect("subsumer queued");
                    let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
                    let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
                }
            }
            // 3. Queue trigger clause `-> D(u0)` (not a certified fact, so general ground Hyper runs).
            let _ = x.store.add(Clause::new(Vec::new(), vec![ld]));
            if let Some(last) = x.store.clauses.last_mut() {
                last.sent = Some(0);
            }
            super::dl_root::register_max(&ix, root, &mut st, &[], &mut setup_sink);
            st
        };

        // 1. Unbounded peak check: admitted peak must cover both the conjunction batch and
        //    the subsequent general ground Hyper product + prune scratch.
        for is_root in [true, false] {
            let mut st = build_state(true);
            let mut sink = BudgetSink {
                run: &run,
                cap: usize::MAX,
                live: 0,
                peak: 0,
                stopped: false,
            };
            let (processed, sample) = measure(|| {
                if is_root {
                    super::dl_root::flush(&ix, &mut st, &mut sink, 1)
                } else {
                    super::dl_rules::flush(&ix, d_cls, &mut st, &mut sink, 1)
                }
            });
            assert_eq!(processed, 1);
            assert!(
                sink.peak >= sample.peak,
                "share={share}, is_root={is_root}: conj+product peak charge ({}) < measured peak ({})",
                sink.peak,
                sample.peak
            );
            assert!(
                sink.live <= 4096,
                "share={share}, is_root={is_root}: leaked reservation ({} B)",
                sink.live
            );
        }

        // 2. Tight cap that grants the 513-partner conjunction batch (~70 KiB) but refuses the
        //    513-partner general ground Hyper product (~70+ KiB): must stop before building
        //    `lists` or growing `Found::prods`.
        let conj_batch_bytes = 513
            * (std::mem::size_of::<(super::dl_rules::Side, &'static str)>()
                + 9 * std::mem::size_of::<Lit>());
        let mid_cap = conj_batch_bytes + (16 << 10);
        for is_root in [true, false] {
            let mut st_mid = build_state(false);
            let mut tight = BudgetSink {
                run: &run,
                cap: mid_cap,
                live: 0,
                peak: 0,
                stopped: false,
            };
            let (_, sample) = measure(|| {
                if is_root {
                    super::dl_root::flush(&ix, &mut st_mid, &mut tight, 1)
                } else {
                    super::dl_rules::flush(&ix, d_cls, &mut st_mid, &mut tight, 1)
                }
            });
            assert!(
                tight.stopped,
                "share={share}, is_root={is_root}: mid cap ({mid_cap}) must refuse general ground Hyper product"
            );
            assert!(
                super::dl_rules::dl(&st_mid).is_some_and(|x| x.store.interrupted),
                "share={share}, is_root={is_root}: product refusal must mark store interrupted"
            );
            assert!(
                sample.peak <= mid_cap + 4096,
                "share={share}, is_root={is_root}: general ground Hyper product overshot mid cap ({mid_cap}): measured peak = {}",
                sample.peak
            );
            assert!(
                tight.peak >= sample.peak,
                "share={share}, is_root={is_root}: admitted peak ({}) < measured peak ({}) under mid cap",
                tight.peak,
                sample.peak
            );
        }
    }
}

/// Sol r4 P1 #1: ordinary `pred` and rooted `root_pred` reached through `pred_c` must
/// count borrowed premises and refuse a tight first-product cap before materializing
/// partner lists, edge snapshots, or cloned `Clause` buffers.
#[test]
fn pred_and_root_pred_refuse_first_product_before_materializing_premises() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    let mut body = String::new();
    for i in 0..520 {
        body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    body.push_str(
        "Declaration(Class(:A))\n\
         Declaration(Class(:B))\n\
         Declaration(Class(:C))\n\
         Declaration(Class(:Seed))\n\
         Declaration(ObjectProperty(:r))\n\
         SubClassOf(:Seed ObjectSomeValuesFrom(:r :C))\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         ClassAssertion(:Seed :u0)\n",
    );
    let input = axioms(&body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let allowed = super::dl_norm::Allowed {
        rooted: true,
        ..Default::default()
    };
    let mut dl_ax = super::dl_norm::normalize(&outside, &mut it, &mut ax, None, &|| false, allowed)
        .expect("dl_norm")
        .map(|(_, d)| d)
        .unwrap_or_default();
    let root = dl_ax.root.unwrap_or_else(|| {
        let r = it.fresh_class();
        dl_ax.root = Some(r);
        r
    });
    for set in &mut ax.disjoint {
        set.sort_unstable();
    }
    ax.dl = Some(Box::new(dl_ax));
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    let dx = ix.dl.as_deref().expect("rooted dl index");
    let a_cls = it.class(&Class::new("urn:tierd1291:A"));
    let b_cls = it.class(&Class::new("urn:tierd1291:B"));
    let c_cls = it.class(&Class::new("urn:tierd1291:C"));
    let seed_cls = it.class(&Class::new("urn:tierd1291:Seed"));
    let h_cls: Vec<Ex> = (0..520)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let exist_e = (0..it.nodes.len() as Ex)
        .find(|&e| {
            matches!(ix.head(e).node, super::index::Node::Exist(..)) && ix.head(e).succ == c_cls
        })
        .expect("existential restriction for :r :C");
    let u0 = dx.consts[0].0;
    let hyp_w = dx.hyp[&exist_e];
    let inv = |r: super::index::Role| dx.inv(&ix, r);

    let run = DlRun::new(false, Default::default()).with_shared_facts(true);
    let cid = run
        .table
        .push(Clause::new(vec![Lit::x(a_cls)], vec![Lit::y(b_cls)]));

    // 1. Ordinary `pred` through `dl_rules::pred_c` with 513 live `F(e, A)` partners.
    let build_ord_state = |subsume_output: bool| {
        let mut st = State::default();
        let x = super::dl_rules::ds(&mut st);
        x.insert_edge(exist_e, c_cls);
        let ground_total = super::dl_clause::ground_total();
        let o = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total,
        };
        let mut max = Vec::new();
        let lf = Lit::f(exist_e, a_cls);
        for i in 0..513usize {
            let mut head = vec![lf];
            for j in 0..7usize {
                head.push(Lit::x(h_cls[(i + j * 73) % 520]));
            }
            let c = Clause::new(vec![Lit::x(h_cls[i])], head);
            let _ = x.store.add(c);
            let id = x.store.peek_live(&run).expect("F(e, A) partner queued");
            let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
            let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
            assert!(max.contains(&lf));
        }
        if subsume_output {
            let _ = x.store.add(Clause::new(Vec::new(), vec![Lit::x(b_cls)]));
            let id = x.store.peek_live(&run).expect("B(x) subsumer queued");
            let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
            let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
        }
        st
    };

    let tight_cap = 4096usize;
    let mut st_ord_tight = build_ord_state(false);
    let mut tight = BudgetSink {
        run: &run,
        cap: tight_cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, sample_ord_tight) = measure(|| {
        super::dl_rules::pred_c(&ix, seed_cls, &mut st_ord_tight, c_cls, cid, &mut tight)
    });
    assert!(
        tight.stopped,
        "ordinary pred_c must refuse the first 513-partner product under tight cap"
    );
    assert!(
        super::dl_rules::dl(&st_ord_tight).is_some_and(|x| x.store.interrupted),
        "ordinary pred_c refusal must mark store interrupted"
    );
    assert!(
        sample_ord_tight.peak <= tight_cap + 4096,
        "ordinary pred_c materialized premises before admission: peak = {} B > cap + 4096 ({})",
        sample_ord_tight.peak,
        tight_cap + 4096
    );

    let mut st_ord_unbounded = build_ord_state(true);
    let mut unbounded = BudgetSink {
        run: &run,
        cap: usize::MAX,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (ok, sample_ord_unbounded) = measure(|| {
        super::dl_rules::pred_c(
            &ix,
            seed_cls,
            &mut st_ord_unbounded,
            c_cls,
            cid,
            &mut unbounded,
        )
    });
    assert!(ok);
    assert!(
        unbounded.peak >= sample_ord_unbounded.peak,
        "ordinary pred_c admitted peak ({}) < measured peak ({})",
        unbounded.peak,
        sample_ord_unbounded.peak
    );
    assert!(
        unbounded.live <= 4096,
        "ordinary pred_c leaked reservation ({} B)",
        unbounded.live
    );

    // 2. Rooted `root_pred` through `dl_rules::pred_c` with 513 live `A(f_e(u0))` partners.
    let mk_xl = |xl: XLit| match run.lits.lit(xl, inv) {
        Made::Lit(l) => l,
        _ => unreachable!(),
    };
    let la_fc = mk_xl(XLit::Atom(a_cls, RTerm::Fc(exist_e, u0)));
    let lb_u0 = mk_xl(XLit::Atom(b_cls, RTerm::C(u0)));
    let lh_u0: Vec<Lit> = h_cls
        .iter()
        .map(|&c| mk_xl(XLit::Atom(c, RTerm::C(u0))))
        .collect();

    let build_root_state = |subsume_output: bool| {
        let mut st = State::default();
        let mut setup_sink = BudgetSink {
            run: &run,
            cap: usize::MAX,
            live: 0,
            peak: 0,
            stopped: false,
        };
        let x = super::dl_rules::ds(&mut st);
        // Seed first partner and flush once so `at_consts -> succ_at` registers `(exist_e, u0)`
        // in `r.by_succ[hyp_w]` via the real production path.
        let mut first_head = vec![la_fc];
        for j in 0..7usize {
            first_head.push(lh_u0[(j * 73) % 520]);
        }
        let _ = x.store.add(Clause::new(vec![Lit::x(h_cls[0])], first_head));
        let _ = super::dl_root::flush(&ix, &mut st, &mut setup_sink, 1);
        assert!(
            super::dl_rules::dl(&st)
                .and_then(|d| d.root.as_deref())
                .is_some_and(|r| r.edges_to(hyp_w).contains(&(exist_e, u0))),
            "succ_at must register (exist_e, u0) edge to hyp_w"
        );
        let x = super::dl_rules::ds(&mut st);
        let ground_total = super::dl_clause::ground_total();
        let o = Rooted {
            ix: &ix,
            lits: &run.lits,
            inv: &inv,
            ground_total,
        };
        let mut max = Vec::new();
        for i in 1..513usize {
            let mut head = vec![la_fc];
            for j in 0..7usize {
                head.push(lh_u0[(i + j * 73) % 520]);
            }
            let c = Clause::new(vec![Lit::x(h_cls[i])], head);
            let _ = x.store.add(c);
            let id = x.store.peek_live(&run).expect("A(f_e(u0)) partner queued");
            let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
            let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
            assert!(max.contains(&la_fc));
        }
        if subsume_output {
            let _ = x.store.add(Clause::new(Vec::new(), vec![lb_u0]));
            let id = x.store.peek_live(&run).expect("B(u0) subsumer queued");
            let _ = x.store.next_prepare_id(&ix, Some(&o), id, &mut max);
            let _ = x.store.next_commit(&ix, Some(&o), id, &max, &run);
        }
        st
    };

    let mut st_root_tight = build_root_state(false);
    let mut root_tight = BudgetSink {
        run: &run,
        cap: tight_cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, sample_root_tight) = measure(|| {
        super::dl_rules::pred_c(&ix, root, &mut st_root_tight, hyp_w, cid, &mut root_tight)
    });
    assert!(
        root_tight.stopped,
        "rooted pred_c must refuse the first 513-partner product under tight cap"
    );
    assert!(
        super::dl_rules::dl(&st_root_tight).is_some_and(|x| x.store.interrupted),
        "rooted pred_c refusal must mark store interrupted"
    );
    assert!(
        sample_root_tight.peak <= tight_cap + 4096,
        "rooted pred_c materialized premises before admission: peak = {} B > cap + 4096 ({})",
        sample_root_tight.peak,
        tight_cap + 4096
    );

    let mut st_root_unbounded = build_root_state(true);
    let mut root_unbounded = BudgetSink {
        run: &run,
        cap: usize::MAX,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (ok, sample_root_unbounded) = measure(|| {
        super::dl_rules::pred_c(
            &ix,
            root,
            &mut st_root_unbounded,
            hyp_w,
            cid,
            &mut root_unbounded,
        )
    });
    assert!(ok);
    assert!(
        root_unbounded.peak >= sample_root_unbounded.peak,
        "rooted pred_c admitted peak ({}) < measured peak ({})",
        root_unbounded.peak,
        sample_root_unbounded.peak
    );
    assert!(
        root_unbounded.live <= 4096,
        "rooted pred_c leaked reservation ({} B)",
        root_unbounded.live
    );
}

/// Sol r4 P1 #1: `at_most` (and its `on_rl` caller) must count borrowed holder and role
/// partners before materializing premise lists and cover retained setup buffers for their
/// full lifetime across admission, prune, and early refusal.
#[test]
fn at_most_refuses_first_product_before_materializing_partners_and_covers_setup() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    let mut body = String::new();
    for i in 0..520 {
        body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    body.push_str(
        "Declaration(Class(:M))\n\
         Declaration(Class(:C0))\n\
         Declaration(Class(:C1))\n\
         Declaration(Class(:Seed))\n\
         Declaration(ObjectProperty(:r))\n\
         SubClassOf(:M ObjectMaxCardinality(1 :r))\n\
         SubClassOf(:Seed ObjectSomeValuesFrom(:r :C0))\n\
         SubClassOf(:Seed ObjectSomeValuesFrom(:r :C1))\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n",
    );
    let input = axioms(&body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let allowed = super::dl_norm::Allowed {
        counting: true,
        ..Default::default()
    };
    let dl_ax = super::dl_norm::normalize(&outside, &mut it, &mut ax, None, &|| false, allowed)
        .expect("dl_norm")
        .map(|(_, d)| d)
        .unwrap_or_default();
    let (holder_m, _, q_role, _) = *dl_ax
        .atmost
        .first()
        .expect("expected atmost holder entry in dl_ax.atmost");
    ax.dl = Some(Box::new(dl_ax));
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    let dx = ix.dl.as_deref().expect("counting dl index");
    assert!(dx.counting, "expected counting Tier D index");
    let c0_cls = it.class(&Class::new("urn:tierd1291:C0"));
    let c1_cls = it.class(&Class::new("urn:tierd1291:C1"));
    let seed_cls = it.class(&Class::new("urn:tierd1291:Seed"));
    let h_cls: Vec<Ex> = (0..520)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let f0 = (0..it.nodes.len() as Ex)
        .find(|&e| {
            matches!(ix.head(e).node, super::index::Node::Exist(..)) && ix.head(e).succ == c0_cls
        })
        .expect("existential for :r :C0");
    let f1 = (0..it.nodes.len() as Ex)
        .find(|&e| {
            matches!(ix.head(e).node, super::index::Node::Exist(..)) && ix.head(e).succ == c1_cls
        })
        .expect("existential for :r :C1");

    let run = DlRun::new(false, Default::default());

    let build_state = |n_partners: usize, extra_lits: usize| {
        let mut st = State::default();
        let x = super::dl_rules::ds(&mut st);
        // Pre-populate edge for f1 so `process` does not allocate `edges` during the trigger.
        x.insert_edge(f1, c1_cls);
        let mut max = Vec::new();
        // 1. Live `X(holder_m)` holder premises (`X` > `Y`, so `X(holder_m)` is always maximal).
        for i in 0..n_partners {
            let mut head = vec![Lit::x(holder_m)];
            for j in 0..extra_lits {
                head.push(Lit::y(h_cls[(i + j * 7) % 520]));
            }
            let _ = x.store.add(Clause::new(vec![Lit::x(h_cls[i % 520])], head));
            let id = x.store.peek_live(&run).expect("holder partner queued");
            let _ = x.store.next_prepare_id(&ix, None, id, &mut max);
            let _ = x.store.next_commit(&ix, None, id, &max, &run);
            assert!(max.contains(&Lit::x(holder_m)));
        }
        // 2. Live `RL(f0, q_role)` successor premises (`RL` > `Y`).
        let l_rl0 = Lit::rl(f0, q_role);
        for i in 0..n_partners {
            let mut head = vec![l_rl0];
            for j in 0..extra_lits {
                head.push(Lit::y(h_cls[(i + j * 11) % 520]));
            }
            let _ = x
                .store
                .add(Clause::new(vec![Lit::x(h_cls[(i + 1) % 520])], head));
            let id = x.store.peek_live(&run).expect("role partner queued");
            let _ = x.store.next_prepare_id(&ix, None, id, &mut max);
            let _ = x.store.next_commit(&ix, None, id, &max, &run);
            assert!(max.contains(&l_rl0));
        }
        // 3. Queue trigger clause `-> RL(f1, q_role)` which invokes `on_rl -> at_most`.
        let _ = x
            .store
            .add(Clause::new(Vec::new(), vec![Lit::rl(f1, q_role)]));
        st
    };

    // 1. Tight cap refusing before setup/first product on 513 holder + 513 role partners (~115 KiB).
    let tight_cap = 2048usize;
    let mut st_tight = build_state(513, 7);
    let mut tight = BudgetSink {
        run: &run,
        cap: tight_cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, sample_tight) =
        measure(|| super::dl_rules::flush(&ix, seed_cls, &mut st_tight, &mut tight, 1));
    assert!(
        tight.stopped,
        "at_most must refuse the first product under tight cap"
    );
    assert!(
        super::dl_rules::dl(&st_tight).is_some_and(|x| x.store.interrupted),
        "at_most refusal must mark store interrupted"
    );
    assert!(
        sample_tight.peak <= tight_cap + 4096,
        "at_most materialized holder/role partners before admission: peak = {} B > cap + 4096 ({})",
        sample_tight.peak,
        tight_cap + 4096
    );
    assert!(
        tight.live <= 256,
        "tight refusal leaked reservation beyond trigger store index growth ({} B)",
        tight.live
    );

    // 2. Mid cap admitting compact setup metadata (~8 KiB) but refusing the ~115 KiB first product.
    let mid_cap = 24 << 10;
    let mut st_mid = build_state(513, 7);
    let mut mid = BudgetSink {
        run: &run,
        cap: mid_cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, sample_mid) =
        measure(|| super::dl_rules::flush(&ix, seed_cls, &mut st_mid, &mut mid, 1));
    assert!(
        mid.stopped,
        "at_most must refuse the first 513-partner product under mid cap"
    );
    assert!(
        super::dl_rules::dl(&st_mid).is_some_and(|x| x.store.interrupted),
        "at_most mid-cap refusal must mark store interrupted"
    );
    assert!(
        sample_mid.peak <= mid_cap + 4096,
        "at_most overshot mid cap ({mid_cap}): measured peak = {} B",
        sample_mid.peak
    );
    assert!(
        mid.peak >= sample_mid.peak,
        "at_most admitted peak ({}) < measured peak ({}) under mid cap",
        mid.peak,
        sample_mid.peak
    );
    assert_eq!(
        mid.live, tight.live,
        "mid-cap refusal must release 100% of admitted setup reservation"
    );

    // 3. Unbounded cap with 16x16 multi-literal partners: admitted peak must cover setup +
    //    product + prune + drain, and retained `unbounded.live` must match permanent `Store`
    //    growth with zero leaked temporary reservation.
    let mut st_unbounded = build_state(16, 32);
    let before_store_bytes = super::dl_rules::dl(&st_unbounded)
        .expect("dl state")
        .store
        .bytes();
    let mut unbounded = BudgetSink {
        run: &run,
        cap: usize::MAX,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (processed, sample_unbounded) =
        measure(|| super::dl_rules::flush(&ix, seed_cls, &mut st_unbounded, &mut unbounded, 1));
    assert_eq!(processed, 1);
    let after_store_bytes = super::dl_rules::dl(&st_unbounded)
        .expect("dl state")
        .store
        .bytes();
    let store_growth = after_store_bytes.saturating_sub(before_store_bytes);
    assert!(
        unbounded.peak >= sample_unbounded.peak,
        "at_most unbounded admitted peak ({}) < measured peak ({})",
        unbounded.peak,
        sample_unbounded.peak
    );
    assert!(
        unbounded.live <= store_growth + 256,
        "at_most unbounded leaked reservation: live ({}) > store growth ({}) + 256",
        unbounded.live,
        store_growth
    );
}

/// Sol r4 P1 #2: growing `Found::prods` from a full product vector (`cap == 64 -> 128`)
/// with singleton premise lists (`prune_scratch == 0`) must not undercharge the moving
/// realloc peak by overlapping an already-allocated payload with both old and new slot
/// buffers.
#[test]
fn pred_product_vector_growth_from_full_prods_with_singleton_lists_covers_realloc_peak() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    let mut body = String::new();
    for i in 0..520 {
        body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    body.push_str(
        "Declaration(Class(:A))\n\
         Declaration(Class(:C))\n\
         Declaration(Class(:Seed))\n\
         Declaration(ObjectProperty(:r))\n\
         SubClassOf(:Seed ObjectSomeValuesFrom(:r :C))\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n",
    );
    let input = axioms(&body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let dl_ax = super::dl_norm::normalize(
        &outside,
        &mut it,
        &mut ax,
        None,
        &|| false,
        Default::default(),
    )
    .expect("dl_norm")
    .map(|(_, d)| d)
    .unwrap_or_default();
    ax.dl = Some(Box::new(dl_ax));
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    let a_cls = it.class(&Class::new("urn:tierd1291:A"));
    let c_cls = it.class(&Class::new("urn:tierd1291:C"));
    let seed_cls = it.class(&Class::new("urn:tierd1291:Seed"));
    let h_cls: Vec<Ex> = (0..520)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let exist_e = (0..it.nodes.len() as Ex)
        .find(|&e| {
            matches!(ix.head(e).node, super::index::Node::Exist(..)) && ix.head(e).succ == c_cls
        })
        .expect("existential for :r :C");

    let run = DlRun::new(false, Default::default());
    let mut cids = Vec::with_capacity(65);
    for k in 0..65usize {
        // Large head (> 4 KiB) so incoming payload is comparable to/larger than `old_cap_bytes`
        // (64 * 96 = 6144 B) when `Found::prods` grows from cap 64 to cap 128.
        let mut head = Vec::with_capacity(512);
        for j in 0..512usize {
            head.push(Lit::y(h_cls[(k + j) % 520]));
        }
        cids.push(run.table.push(Clause::new(vec![Lit::x(a_cls)], head)));
    }

    let mut st = State::default();
    let x = super::dl_rules::ds(&mut st);
    x.insert_edge(exist_e, c_cls);
    x.pred_in.insert(c_cls, cids);
    // Exactly ONE live partner clause for `F(e, A)` -> singleton premise list (`prune_scratch == 0`).
    let lf = Lit::f(exist_e, a_cls);
    let mut p_head = vec![lf];
    for &hc in &h_cls[..256] {
        p_head.push(Lit::x(hc));
    }
    let _ = x.store.add(Clause::new(vec![Lit::x(h_cls[519])], p_head));
    let mut max = Vec::new();
    let id = x.store.peek_live(&run).expect("singleton partner queued");
    let _ = x.store.next_prepare_id(&ix, None, id, &mut max);
    let _ = x.store.next_commit(&ix, None, id, &max, &run);
    assert!(max.contains(&lf));

    let mut sink = BudgetSink {
        run: &run,
        cap: usize::MAX,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let ((replayed, prods_len), sample) = super::dl_rules::with_late_unit_mode_override(2, || {
        measure(|| {
            let mut guard = super::saturate::Guard::new(&mut sink);
            let mut found = Found::default();
            let ok = super::dl_rules::late(
                &ix,
                seed_cls,
                &mut st,
                exist_e,
                c_cls,
                "realloc-test",
                &mut found,
                &mut guard,
            );
            let len = found.products_len();
            drop(found);
            drop(guard);
            (ok, len)
        })
    });
    assert!(
        replayed,
        "late replay of 65 singleton products must succeed"
    );
    assert_eq!(prods_len, 65, "expected 65 queued products (cap 64 -> 128)");
    assert!(
        sink.peak >= sample.peak,
        "Found::prods growth from full cap=64 with singleton lists undercharged realloc peak: admitted peak ({}) < measured peak ({})",
        sink.peak,
        sample.peak
    );
    assert_eq!(sink.live, 0, "dropping guard must release all reservations");
}

/// Sol r5 P1: conditional triggers with a wide body (and residual head) must pre-admit
/// the working `Side` built by `side(c, l)` before allocating it and keep its bytes live
/// across downstream product and setup reservations in both ordinary `dl_rules::process`
/// and rooted `dl_root::process`.
#[test]
fn wide_conditional_trigger_admits_working_side_in_ordinary_and_rooted_process() {
    struct BudgetSink<'a> {
        run: &'a DlRun,
        cap: usize,
        live: usize,
        peak: usize,
        stopped: bool,
    }
    impl Sink for BudgetSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, _: Concl) {}
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, bytes: usize) -> bool {
            if self.stopped || self.live.saturating_add(bytes) > self.cap {
                self.stopped = true;
                return false;
            }
            self.live += bytes;
            self.peak = self.peak.max(self.live);
            true
        }
        fn release_data(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn admit_dl(&mut self, bytes: usize) -> bool {
            self.reserve_data(bytes)
        }
        fn commit_dl(&mut self, admitted: usize, kept: usize) {
            self.live = self.live.saturating_sub(admitted.saturating_sub(kept));
        }
        fn go_on(&self) -> bool {
            !self.stopped
        }
        fn stopped(&self) -> bool {
            self.stopped
        }
        fn worked(&mut self, _: usize) {}
    }

    // --- Part 1: Ordinary `dl_rules::process` -> `on_rl` -> `at_most` ---
    let mut ord_body = String::new();
    for i in 0..2048 {
        ord_body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    ord_body.push_str(
        "Declaration(Class(:M))\n\
         Declaration(Class(:C0))\n\
         Declaration(Class(:C1))\n\
         Declaration(Class(:Seed))\n\
         Declaration(ObjectProperty(:r))\n\
         SubClassOf(:M ObjectMaxCardinality(1 :r))\n\
         SubClassOf(:Seed ObjectSomeValuesFrom(:r :C0))\n\
         SubClassOf(:Seed ObjectSomeValuesFrom(:r :C1))\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n",
    );
    let input = axioms(&ord_body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let allowed = super::dl_norm::Allowed {
        counting: true,
        ..Default::default()
    };
    let dl_ax = super::dl_norm::normalize(&outside, &mut it, &mut ax, None, &|| false, allowed)
        .expect("dl_norm")
        .map(|(_, d)| d)
        .unwrap_or_default();
    let (holder_m, _, q_role, _) = *dl_ax
        .atmost
        .first()
        .expect("expected atmost holder entry in dl_ax.atmost");
    ax.dl = Some(Box::new(dl_ax));
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();
    let c0_cls = it.class(&Class::new("urn:tierd1291:C0"));
    let c1_cls = it.class(&Class::new("urn:tierd1291:C1"));
    let seed_cls = it.class(&Class::new("urn:tierd1291:Seed"));
    let h_cls: Vec<Ex> = (0..2048)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let f0 = (0..it.nodes.len() as Ex)
        .find(|&e| {
            matches!(ix.head(e).node, super::index::Node::Exist(..)) && ix.head(e).succ == c0_cls
        })
        .expect("existential for :r :C0");
    let f1 = (0..it.nodes.len() as Ex)
        .find(|&e| {
            matches!(ix.head(e).node, super::index::Node::Exist(..)) && ix.head(e).succ == c1_cls
        })
        .expect("existential for :r :C1");
    let run = DlRun::new(false, Default::default());

    let trig_body_len = 2048usize;
    let trig_extra_head = 256usize;
    let ord_c_bytes = (trig_body_len + 1 + trig_extra_head) * std::mem::size_of::<Lit>();
    let build_ord_state = |n_partners: usize, extra_lits: usize| {
        let mut st = State::default();
        let x = super::dl_rules::ds(&mut st);
        x.insert_edge(f1, c1_cls);
        let mut max = Vec::new();
        for i in 0..n_partners {
            let mut head = vec![Lit::x(holder_m)];
            for j in 0..extra_lits {
                head.push(Lit::y(h_cls[(i + j * 7) % 520]));
            }
            let _ = x.store.add(Clause::new(vec![Lit::x(h_cls[i % 520])], head));
            let id = x.store.peek_live(&run).expect("holder partner queued");
            let _ = x.store.next_prepare_id(&ix, None, id, &mut max);
            let _ = x.store.next_commit(&ix, None, id, &max, &run);
        }
        let l_rl0 = Lit::rl(f0, q_role);
        for i in 0..n_partners {
            let mut head = vec![l_rl0];
            for j in 0..extra_lits {
                head.push(Lit::y(h_cls[(i + j * 11) % 520]));
            }
            let _ = x
                .store
                .add(Clause::new(vec![Lit::x(h_cls[(i + 1) % 520])], head));
            let id = x.store.peek_live(&run).expect("role partner queued");
            let _ = x.store.next_prepare_id(&ix, None, id, &mut max);
            let _ = x.store.next_commit(&ix, None, id, &max, &run);
        }
        // Wide conditional trigger: 2048 body lits + 256 residual Y head lits + maximal RL(f1, q).
        let body: Vec<Lit> = h_cls.iter().map(|&c| Lit::x(c)).collect();
        let mut head = Vec::with_capacity(1 + trig_extra_head);
        head.push(Lit::rl(f1, q_role));
        for &hc in &h_cls[..trig_extra_head] {
            head.push(Lit::y(hc));
        }
        let _ = x.store.add(Clause::new(body, head));
        st
    };

    // 1a. Tight cap admitting working `Clause` (~9.2 KiB) + `max` + store commit, refusing `prem`.
    let ord_tight_cap = ord_c_bytes + 4096;
    let mut st_ord_tight = build_ord_state(513, 7);
    let mut ord_tight = BudgetSink {
        run: &run,
        cap: ord_tight_cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, sample_ord_tight) =
        measure(|| super::dl_rules::flush(&ix, seed_cls, &mut st_ord_tight, &mut ord_tight, 1));
    assert!(
        ord_tight.stopped,
        "ordinary wide conditional trigger must stop under tight cap"
    );
    assert!(
        super::dl_rules::dl(&st_ord_tight).is_some_and(|x| x.store.interrupted),
        "ordinary wide conditional trigger refusal must mark store interrupted"
    );
    assert!(
        sample_ord_tight.peak <= ord_tight_cap + 1024,
        "ordinary process allocated uncharged working Side past tight cap ({ord_tight_cap}): measured peak = {} B",
        sample_ord_tight.peak
    );
    assert!(
        ord_tight.peak >= sample_ord_tight.peak,
        "ordinary tight cap admitted peak ({}) < measured peak ({})",
        ord_tight.peak,
        sample_ord_tight.peak
    );

    // 1b. Mid cap admitting working `Clause` + `prem` + compact `refs`/`cands`, refusing product.
    let ord_prem_bytes = (trig_body_len + trig_extra_head) * std::mem::size_of::<Lit>();
    let ord_mid_cap = ord_c_bytes + ord_prem_bytes + (20 << 10);
    let mut st_ord_mid = build_ord_state(513, 7);
    let mut ord_mid = BudgetSink {
        run: &run,
        cap: ord_mid_cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, sample_ord_mid) =
        measure(|| super::dl_rules::flush(&ix, seed_cls, &mut st_ord_mid, &mut ord_mid, 1));
    assert!(
        ord_mid.stopped,
        "ordinary wide conditional trigger must refuse first ~115 KiB product under mid cap"
    );
    assert!(
        super::dl_rules::dl(&st_ord_mid).is_some_and(|x| x.store.interrupted),
        "ordinary mid-cap refusal must mark store interrupted"
    );
    assert!(
        ord_mid.peak >= sample_ord_mid.peak,
        "ordinary mid-cap admitted peak ({}) < measured peak ({}) (working Side uncharged alongside setup)",
        ord_mid.peak,
        sample_ord_mid.peak
    );
    assert!(
        sample_ord_mid.peak <= ord_mid_cap + 4096,
        "ordinary mid-cap measured peak ({}) overshot cap ({ord_mid_cap})",
        sample_ord_mid.peak
    );
    assert_eq!(
        ord_mid.live, ord_tight.live,
        "ordinary mid-cap refusal must release 100% of working Side and setup reservations"
    );

    // 1c. Unbounded cap with wide conditional trigger (8x8 partners): peak covered, zero leak.
    let mut st_ord_unbounded = build_ord_state(8, 8);
    let before_store_bytes = super::dl_rules::dl(&st_ord_unbounded)
        .expect("dl state")
        .store
        .bytes();
    let mut ord_unbounded = BudgetSink {
        run: &run,
        cap: usize::MAX,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (processed, sample_ord_unbounded) = measure(|| {
        super::dl_rules::flush(&ix, seed_cls, &mut st_ord_unbounded, &mut ord_unbounded, 1)
    });
    assert_eq!(processed, 1);
    let after_store_bytes = super::dl_rules::dl(&st_ord_unbounded)
        .expect("dl state")
        .store
        .bytes();
    let store_growth = after_store_bytes.saturating_sub(before_store_bytes);
    assert!(
        ord_unbounded.peak >= sample_ord_unbounded.peak,
        "ordinary unbounded admitted peak ({}) < measured peak ({})",
        ord_unbounded.peak,
        sample_ord_unbounded.peak
    );
    assert!(
        ord_unbounded.live <= store_growth + 256,
        "ordinary unbounded leaked reservation: live ({}) > store growth ({}) + 256",
        ord_unbounded.live,
        store_growth
    );

    // --- Part 2: Rooted `dl_root::process` -> `at_consts` -> `root_pred` ---
    let mut root_body = String::new();
    for i in 0..2048 {
        root_body.push_str(&format!("Declaration(Class(:H{i}))\n"));
    }
    root_body.push_str(
        "Declaration(Class(:A))\n\
         Declaration(Class(:A2))\n\
         Declaration(Class(:B))\n\
         Declaration(Class(:C))\n\
         Declaration(Class(:Seed))\n\
         Declaration(ObjectProperty(:r))\n\
         SubClassOf(:Seed ObjectSomeValuesFrom(:r :C))\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         ClassAssertion(:Seed :u0)\n",
    );
    let r_input = axioms(&root_body);
    let mut r_it = Interner::new();
    let mut r_ax = Axioms::default();
    let mut r_outside = Vec::new();
    for a in &r_input {
        if super::index::normalize(&a.kind, &mut r_it, &mut r_ax).is_some() {
            r_outside.push(&a.kind);
        }
    }
    let r_allowed = super::dl_norm::Allowed {
        rooted: true,
        ..Default::default()
    };
    let mut r_dl_ax =
        super::dl_norm::normalize(&r_outside, &mut r_it, &mut r_ax, None, &|| false, r_allowed)
            .expect("dl_norm")
            .map(|(_, d)| d)
            .unwrap_or_default();
    if r_dl_ax.root.is_none() {
        r_dl_ax.root = Some(r_it.fresh_class());
    }
    for set in &mut r_ax.disjoint {
        set.sort_unstable();
    }
    r_ax.dl = Some(Box::new(r_dl_ax));
    let r_ix = Index::build(&r_ax, &mut r_it, None, &|| false).unwrap();
    let r_dx = r_ix.dl.as_deref().expect("rooted dl index");
    let a_cls = r_it.class(&Class::new("urn:tierd1291:A"));
    let a2_cls = r_it.class(&Class::new("urn:tierd1291:A2"));
    let b_cls = r_it.class(&Class::new("urn:tierd1291:B"));
    let c_cls = r_it.class(&Class::new("urn:tierd1291:C"));
    let rh_cls: Vec<Ex> = (0..2048)
        .map(|i| r_it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let exist_e = (0..r_it.nodes.len() as Ex)
        .find(|&e| {
            matches!(r_ix.head(e).node, super::index::Node::Exist(..)) && r_ix.head(e).succ == c_cls
        })
        .expect("existential restriction for :r :C");
    let u0 = r_dx.consts[0].0;
    let hyp_w = r_dx.hyp[&exist_e];
    let inv = |r: super::index::Role| r_dx.inv(&r_ix, r);
    let r_run = DlRun::new(false, Default::default()).with_shared_facts(true);
    let mk_xl = |xl: XLit| match r_run.lits.lit(xl, inv) {
        Made::Lit(l) => l,
        _ => unreachable!(),
    };
    let la_fc = mk_xl(XLit::Atom(a_cls, RTerm::Fc(exist_e, u0)));
    let la2_fc = mk_xl(XLit::Atom(a2_cls, RTerm::Fc(exist_e, u0)));
    let lh_u0: Vec<Lit> = rh_cls[..520]
        .iter()
        .map(|&c| mk_xl(XLit::Atom(c, RTerm::C(u0))))
        .collect();
    // Successor clause: A(x) ∧ A2(x) -> B(y). Trigger fixes A(f_e(u0)), leaving 513 A2 partners.
    let cid = r_run.table.push(Clause::new(
        vec![Lit::x(a_cls), Lit::x(a2_cls)],
        vec![Lit::y(b_cls)],
    ));

    let root_c_bytes = (2048 + 1) * std::mem::size_of::<Lit>();
    let root_prem_bytes = 2048 * std::mem::size_of::<Lit>();
    let build_root_state = || {
        let mut st = State::default();
        let mut setup_sink = BudgetSink {
            run: &r_run,
            cap: usize::MAX,
            live: 0,
            peak: 0,
            stopped: false,
        };
        let x = super::dl_rules::ds(&mut st);
        // Seed edge `(exist_e, u0)` to `hyp_w` and mark `(hyp_w, a_cls)` already sent.
        let _ = x.store.add(Clause::new(Vec::new(), vec![la_fc]));
        let _ = super::dl_root::flush(&r_ix, &mut st, &mut setup_sink, 1);
        let x = super::dl_rules::ds(&mut st);
        x.store.clauses[0].dead = true;
        x.pred_in.insert(hyp_w, vec![cid]);
        let ground_total = super::dl_clause::ground_total();
        let o = Rooted {
            ix: &r_ix,
            lits: &r_run.lits,
            inv: &inv,
            ground_total,
        };
        let mut max = Vec::new();
        for i in 0..513usize {
            let mut head = vec![la2_fc];
            for j in 0..7usize {
                head.push(lh_u0[(i + j * 73) % 520]);
            }
            let _ = x.store.add(Clause::new(vec![Lit::x(rh_cls[i])], head));
            let id = x
                .store
                .peek_live(&r_run)
                .expect("A2(f_e(u0)) partner queued");
            let _ = x.store.next_prepare_id(&r_ix, Some(&o), id, &mut max);
            let _ = x.store.next_commit(&r_ix, Some(&o), id, &max, &r_run);
            assert!(max.contains(&la2_fc));
        }
        // Wide conditional trigger: 2048 non-ground X body lits -> A(f_e(u0)).
        let body: Vec<Lit> = rh_cls.iter().map(|&c| Lit::x(c)).collect();
        let _ = x.store.add(Clause::new(body, vec![la_fc]));
        st
    };

    // 2a. Tight cap admitting working `Clause` (~8.2 KiB) + `max` + store commit, refusing `prem`.
    let root_tight_cap = root_c_bytes + 4096;
    let mut st_root_tight = build_root_state();
    let mut root_tight = BudgetSink {
        run: &r_run,
        cap: root_tight_cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, sample_root_tight) =
        measure(|| super::dl_root::flush(&r_ix, &mut st_root_tight, &mut root_tight, 1));
    assert!(
        root_tight.stopped,
        "rooted wide conditional trigger must stop under tight cap"
    );
    assert!(
        super::dl_rules::dl(&st_root_tight).is_some_and(|x| x.store.interrupted),
        "rooted tight-cap refusal must mark store interrupted"
    );
    assert!(
        sample_root_tight.peak <= root_tight_cap + 1024,
        "rooted process allocated uncharged working Side past tight cap ({root_tight_cap}): measured peak = {} B",
        sample_root_tight.peak
    );
    assert!(
        root_tight.peak >= sample_root_tight.peak,
        "rooted tight-cap admitted peak ({}) < measured peak ({})",
        root_tight.peak,
        sample_root_tight.peak
    );

    // 2b. Mid cap admitting working `Clause` + `prem`, refusing the ~70 KiB first `root_pred` product.
    let root_mid_cap = root_c_bytes + root_prem_bytes + 8192;
    let mut st_root_mid = build_root_state();
    let mut root_mid = BudgetSink {
        run: &r_run,
        cap: root_mid_cap,
        live: 0,
        peak: 0,
        stopped: false,
    };
    let (_, sample_root_mid) =
        measure(|| super::dl_root::flush(&r_ix, &mut st_root_mid, &mut root_mid, 1));
    assert!(
        root_mid.stopped,
        "rooted wide conditional trigger must refuse first ~70 KiB product under mid cap"
    );
    assert!(
        super::dl_rules::dl(&st_root_mid).is_some_and(|x| x.store.interrupted),
        "rooted mid-cap refusal must mark store interrupted"
    );
    assert!(
        root_mid.peak >= sample_root_mid.peak,
        "rooted mid-cap admitted peak ({}) < measured peak ({}) (working Side uncharged)",
        root_mid.peak,
        sample_root_mid.peak
    );
    assert!(
        sample_root_mid.peak <= root_mid_cap + 4096,
        "rooted mid-cap measured peak ({}) overshot cap ({root_mid_cap})",
        sample_root_mid.peak
    );
    assert_eq!(
        root_mid.live, root_tight.live,
        "rooted mid-cap refusal must release 100% of working Side reservation"
    );
}

/// Sol r5 P2: for a non-hypothetical successor, `pred_c` must visit reaching edges in
/// ascending restriction-ID union order (merging explicit edges and unit edges), so a
/// lower-ID unit edge with a singleton product delivers its conclusion before a higher-ID
/// explicit edge whose wide product hits the width limit.
#[test]
fn pred_c_visits_reaching_edges_in_id_order_before_width_stop() {
    struct CollectSink<'a> {
        run: &'a DlRun,
        me_concls: Vec<Concl>,
    }
    impl Sink for CollectSink<'_> {
        fn send(&mut self, _: Ex, _: Concl) {}
        fn me(&mut self, c: Concl) {
            self.me_concls.push(c);
        }
        fn dl(&self) -> &DlRun {
            self.run
        }
        fn reserve_data(&mut self, _: usize) -> bool {
            true
        }
        fn release_data(&mut self, _: usize) {}
        fn admit_dl(&mut self, _: usize) -> bool {
            true
        }
        fn commit_dl(&mut self, _: usize, _: usize) {}
        fn go_on(&self) -> bool {
            true
        }
        fn stopped(&self) -> bool {
            false
        }
        fn worked(&mut self, _: usize) {}
    }

    let body = "Declaration(Class(:A1))\n\
                Declaration(Class(:A2))\n\
                Declaration(Class(:B))\n\
                Declaration(Class(:C))\n\
                Declaration(Class(:Seed))\n\
                Declaration(Class(:H0))\n\
                Declaration(Class(:H1))\n\
                Declaration(Class(:H2))\n\
                Declaration(Class(:H3))\n\
                Declaration(ObjectProperty(:r1))\n\
                Declaration(ObjectProperty(:r2))\n\
                SubClassOf(:Seed ObjectSomeValuesFrom(:r1 :C))\n\
                SubClassOf(:Seed ObjectSomeValuesFrom(:r2 :C))\n\
                SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n";
    let input = axioms(body);
    let mut it = Interner::new();
    let mut ax = Axioms::default();
    let mut outside = Vec::new();
    for a in &input {
        if super::index::normalize(&a.kind, &mut it, &mut ax).is_some() {
            outside.push(&a.kind);
        }
    }
    let dl_ax = super::dl_norm::normalize(
        &outside,
        &mut it,
        &mut ax,
        None,
        &|| false,
        Default::default(),
    )
    .expect("dl_norm")
    .map(|(_, d)| d)
    .unwrap_or_default();
    ax.dl = Some(Box::new(dl_ax));
    let ix = Index::build(&ax, &mut it, None, &|| false).unwrap();

    let a1_cls = it.class(&Class::new("urn:tierd1291:A1"));
    let a2_cls = it.class(&Class::new("urn:tierd1291:A2"));
    let b_cls = it.class(&Class::new("urn:tierd1291:B"));
    let c_cls = it.class(&Class::new("urn:tierd1291:C"));
    let seed_cls = it.class(&Class::new("urn:tierd1291:Seed"));
    let h_cls: Vec<Ex> = (0..4)
        .map(|i| it.class(&Class::new(format!("urn:tierd1291:H{i}"))))
        .collect();
    let mut exist_to_c: Vec<Ex> = (0..it.nodes.len() as Ex)
        .filter(|&e| {
            matches!(ix.head(e).node, super::index::Node::Exist(..)) && ix.head(e).succ == c_cls
        })
        .collect();
    exist_to_c.sort_unstable();
    assert_eq!(
        exist_to_c.len(),
        2,
        "expected two existential restrictions reaching :C"
    );
    let (e_low, e_high) = (exist_to_c[0], exist_to_c[1]);
    assert!(e_low < e_high);

    let run = DlRun::new(false, Default::default());
    run.set_width_limit(2);
    let cid = run.table.push(Clause::new(
        vec![Lit::x(a1_cls), Lit::x(a2_cls)],
        vec![Lit::y(b_cls)],
    ));

    let mut st = State::default();
    // Lower-ID restriction is a unit edge; higher-ID restriction is an explicit edge.
    st.subs.insert(e_low);
    let x = super::dl_rules::ds(&mut st);
    x.insert_edge(e_high, c_cls);

    let mut max = Vec::new();
    let mut commit = |c: Clause| {
        let _ = x.store.add(c);
        let id = x.store.peek_live(&run).expect("clause queued");
        let _ = x.store.next_prepare_id(&ix, None, id, &mut max);
        let _ = x.store.next_commit(&ix, None, id, &max, &run);
    };
    // Lower-ID unit edge `e_low`: singleton product (1 * 1 = 1 <= 2) deriving unit `-> B(x)`.
    commit(Clause::new(Vec::new(), vec![Lit::f(e_low, a1_cls)]));
    commit(Clause::new(Vec::new(), vec![Lit::f(e_low, a2_cls)]));
    // Higher-ID explicit edge `e_high`: wide product (2 * 2 = 4 > 2) exceeding width limit.
    commit(Clause::new(
        vec![Lit::x(h_cls[0])],
        vec![Lit::f(e_high, a1_cls)],
    ));
    commit(Clause::new(
        vec![Lit::x(h_cls[1])],
        vec![Lit::f(e_high, a1_cls)],
    ));
    commit(Clause::new(
        vec![Lit::x(h_cls[2])],
        vec![Lit::f(e_high, a2_cls)],
    ));
    commit(Clause::new(
        vec![Lit::x(h_cls[3])],
        vec![Lit::f(e_high, a2_cls)],
    ));

    let mut sink = CollectSink {
        run: &run,
        me_concls: Vec::new(),
    };
    let ok = super::dl_rules::pred_c(&ix, seed_cls, &mut st, c_cls, cid, &mut sink);
    assert!(ok);
    assert!(
        sink.me_concls.contains(&Concl::Sub(b_cls)),
        "lower-ID unit edge's singleton conclusion `-> B(x)` must be delivered before higher-ID explicit edge triggers width-limit stop; got {:?}",
        sink.me_concls
    );
    assert!(
        super::dl_rules::dl(&st).is_some_and(|x| x.store.interrupted),
        "higher-ID explicit edge's wide product must still trigger width-limit interruption"
    );
}

/// Phase 2 `Merge` UF + `CanonMap` exact accounting must cover actual counting-allocator
/// live heap, including many aliases sharing one long representative, long-IRI pairs, and
/// long anonymous blank labels.
#[test]
fn merge_canon_and_uf_cover_counting_allocator_heap() {
    let long_rep = NamedIndividual::new(format!("http://example.org/rep/{}", "r".repeat(1024)));
    let mut pairs: Vec<(Individual, Individual)> = Vec::new();
    // 500 aliases sharing one 1024-byte representative.
    for i in 0..500usize {
        let alias = NamedIndividual::new(format!(
            "http://example.org/z-alias-{i:04}/{}",
            "a".repeat(256)
        ));
        pairs.push((
            Individual::Named(long_rep.clone()),
            Individual::Named(alias),
        ));
    }
    // 200 distinct long-IRI pairs (512-byte IRIs).
    for i in 0..200usize {
        let a = NamedIndividual::new(format!("http://example.org/p{i:04}a/{}", "x".repeat(512)));
        let b = NamedIndividual::new(format!("http://example.org/p{i:04}b/{}", "y".repeat(512)));
        pairs.push((Individual::Named(a), Individual::Named(b)));
    }
    // Long anonymous blank label merged with a named individual.
    let long_blank = AnonymousIndividual::new(format!("blank_{}", "b".repeat(1024)));
    pairs.push((
        Individual::Named(long_rep.clone()),
        Individual::Anonymous(long_blank),
    ));

    let limits = Limits::default();
    let mut peak_admitted = 0usize;
    let mut admit = |used: usize| -> bool {
        peak_admitted = peak_admitted.max(used);
        true
    };
    let ((uf_bytes, canon_bytes, canon_len), sample) = measure(|| {
        let mut m = Merge::default();
        for (a, b) in &pairs {
            m.union_individual(a, b, 0, &mut admit).expect("admitted");
        }
        let uf_bytes = m.uf_bytes();
        let canon = m
            .build_canon(0, &limits, &mut admit)
            .ok()
            .flatten()
            .expect("non-empty canon");
        let canon_bytes = canon.owned_bytes();
        let canon_len = canon.len();
        (uf_bytes, canon_bytes, canon_len)
    });

    assert_eq!(canon_len, 500 + 200 + 1);
    assert!(
        uf_bytes + canon_bytes >= sample.live,
        "uf_bytes ({uf_bytes}) + canon_bytes ({canon_bytes}) undercounted live heap ({})",
        sample.live
    );
    assert!(
        peak_admitted >= sample.peak,
        "peak_admitted ({peak_admitted}) undercounted measured peak heap ({})",
        sample.peak
    );

    // End-to-end classification with long-IRI SameIndividual + entailed merge must cover peak.
    let mut body = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(:C0 ObjectOneOf(:e1))\n\
         SubClassOf(ObjectOneOf(:e0 :e1) :C0)\n",
    );
    let rep_name = format!("rep_{}", "r".repeat(512));
    for i in 0..80usize {
        let alias = format!("z_{i:03}_{}", "a".repeat(256));
        body.push_str(&format!("SameIndividual(:{rep_name} :{alias})\n"));
    }
    body.push_str(&format!("ClassAssertion(:C0 :{rep_name})\n"));
    let input = axioms(&body);
    let mut k = Classifier::of(&input);
    let (res, run_sample) = measure(|| {
        k.classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
    });
    let out = res.expect("classify");
    assert!(out.complete);
    assert!(out.stats.dl_rooted_restarts >= 1);
    assert!(out.stats.dl_rooted_merged >= 81);
    assert!(
        out.stats.peak_memory >= run_sample.peak,
        "peak_memory ({}) < measured peak ({})",
        out.stats.peak_memory,
        run_sample.peak
    );
}

/// Optional prepass (`collect_same`) refusal must discard provisional partition/map,
/// disable merging for the call, answer cleanly with the ordinary classifier, and leave a
/// follow-up unrestricted classify on the same `Classifier` identical to a fresh one.
#[test]
fn merge_prepass_charge_refusal_disables_merge_and_answers_normally() {
    let mut body = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(:A :B)\n",
    );
    let rep = format!("r_{}", "x".repeat(512));
    for i in 0..40usize {
        body.push_str(&format!(
            "SameIndividual(:{rep} :z_{i:02}_{})\n",
            "y".repeat(512)
        ));
    }
    body.push_str(&format!("ClassAssertion(:A :{rep})\n"));
    let input = axioms(&body);

    // Refuse the first non-zero charge during `collect_same` UF/CanonMap build, then allow
    // subsequent ordinary classifier charges so the fallback finishes.
    let refused = Arc::new(AtomicBool::new(false));
    let refused_cb = refused.clone();
    let charge = Arc::new(move |bytes: usize| -> bool {
        if !refused_cb.swap(true, Ordering::SeqCst) && bytes > 0 {
            return false;
        }
        true
    });

    let mut k = Classifier::of(&input);
    let out = k
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            charge: Some(charge),
            ..Limits::default()
        })
        .expect("ordinary classifier should succeed when prepass declines");
    assert!(refused.load(Ordering::SeqCst), "prepass charge was hit");
    assert_eq!(
        out.stats.dl_rooted_merged, 0,
        "merging disabled after refusal"
    );
    assert!(out.taxonomy.is_subclass(
        &Class::new("urn:tierd1291:A"),
        &Class::new("urn:tierd1291:B")
    ));

    // Follow-up unrestricted classify on the same Classifier must match a fresh one.
    let follow = k
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .expect("follow-up classify");
    assert_eq!(follow.stats.dl_rooted_merged, 40);

    let mut fresh = Classifier::of(&input);
    let expected = fresh
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .expect("fresh classify");
    assert_eq!(follow.consistent, expected.consistent);
    assert_eq!(
        follow.stats.dl_rooted_merged,
        expected.stats.dl_rooted_merged
    );
}

/// Refusal during harvest/restart allocation must route through ordinary memory handling
/// (clearing `dl_worked` so `dl_rooted_over` is not falsely claimed), stay bounded, and
/// recover cleanly on a follow-up unrestricted call on the same `Classifier`.
#[test]
fn merge_restart_and_harvest_refusal_reclaims_and_routes_to_memory_fallback() {
    let mut body = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(:C0 ObjectOneOf(:a2))\n\
         SubClassOf(ObjectOneOf(:a0 :a2) :C0)\n\
         ClassAssertion(:C0 :a0)\n",
    );
    // Add long-IRI derived equalities so restart union/CanonMap allocates noticeably.
    for i in 0..16usize {
        let a = format!("u{i:02}_{}", "p".repeat(512));
        let b = format!("v{i:02}_{}", "q".repeat(512));
        body.push_str(&format!(
            "SubClassOf(:K{i} ObjectOneOf(:{b}))\n\
             SubClassOf(ObjectOneOf(:{a} :{b}) :K{i})\n"
        ));
    }
    let input = axioms(&body);

    // First measure baseline unrestricted peak to calibrate a tight charge cap that lets
    // prefix saturation start and refuses during harvest/restart.
    let mut base_k = Classifier::of(&input);
    let base_out = base_k
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .expect("unrestricted");
    assert!(base_out.stats.dl_rooted_restarts >= 1);

    // Sweep several tight caps below `base_out.stats.peak_memory` so we exercise refusal
    // during harvest and restart rebuilding.
    let mut saw_memory_path = false;
    for frac in [1usize, 2, 3, 4] {
        let cap = (base_out.stats.peak_memory * frac) / 6;
        let mut k = Classifier::of(&input);
        let (res, sample) = measure(|| {
            k.classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge: true,
                max_memory: Some(cap),
                ..Limits::default()
            })
        });
        match res {
            Ok(out) => {
                assert!(
                    !out.stats.dl_rooted_over,
                    "memory refusal must not masquerade as rooted work exhaustion"
                );
                if out.stats.dl_over_budget || out.stats.horn_over_budget {
                    saw_memory_path = true;
                }
            }
            Err(Error::Memory { .. }) => {
                saw_memory_path = true;
            }
            Err(other) => panic!("unexpected error under tight memory cap: {other:?}"),
        }
        assert!(
            sample.peak <= cap + (512 << 10),
            "tight cap measured peak ({}) overshot cap ({cap})",
            sample.peak
        );
        // Follow-up unrestricted classify on the same Classifier must succeed cleanly.
        let recovered = k
            .classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge: true,
                ..Limits::default()
            })
            .expect("follow-up unrestricted classify");
        assert!(recovered.complete);
        assert_eq!(recovered.consistent, base_out.consistent);
        assert_eq!(
            recovered.stats.dl_rooted_merged,
            base_out.stats.dl_rooted_merged
        );
    }
    assert!(
        saw_memory_path,
        "expected at least one cap to trigger memory refusal"
    );
}

/// Cancellation during `collect_same` (including inside a single large n-ary
/// `SameIndividual` axiom) and during saturation/restart must return `Error::Cancelled`
/// and invalidate cached answers on `Classifier`.
#[test]
fn merge_cancellation_polls_n_ary_same_and_invalidates_old_answers() {
    let mut k = Classifier::of(&axioms("SubClassOf(:A :B)\nClassAssertion(:A :a0)\n"));
    let first = k
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .expect("initial classify");
    assert!(first.consistent);
    assert!(k.describe(&Class::new("urn:tierd1291:A")).is_some());

    // Add a single very large n-ary SameIndividual axiom (> POLL_AXIOMS members).
    let mut members = std::collections::BTreeSet::new();
    for i in 0..300usize {
        members.insert(Individual::Named(NamedIndividual::new(format!(
            "urn:tierd1291:n{i:04}"
        ))));
    }
    k.add([&Axiom::new(AxiomKind::SameIndividual(members))]);
    k.add([&Axiom::sub_class_of(
        Class::new("urn:tierd1291:B"),
        Class::new("urn:tierd1291:C"),
    )]);

    // Cancel after 2 poll checks so it fires mid-loop inside the single n-ary axiom.
    let polls = Arc::new(AtomicUsize::new(0));
    let polls_cb = polls.clone();
    let err = k
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            cancelled: Some(Arc::new(move || {
                polls_cb.fetch_add(1, Ordering::SeqCst) >= 2
            })),
            ..Limits::default()
        })
        .expect_err("must cancel inside large n-ary SameIndividual");
    assert_eq!(err, Error::Cancelled);
    assert!(
        k.describe(&Class::new("urn:tierd1291:A")).is_none(),
        "cancelled pre-saturation preparation must invalidate old describe() answers"
    );

    // Follow-up unrestricted classify must reflect the added axioms (A ⊑ C) even though
    // `Classifier::run_held` advanced its edit counters on the cancelled call!
    let after = k
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .expect("follow-up classify after cancel");
    assert!(after.taxonomy.is_subclass(
        &Class::new("urn:tierd1291:A"),
        &Class::new("urn:tierd1291:C")
    ));
    assert_eq!(after.stats.dl_rooted_merged, 299);
}

/// Logical allowance (`allowance_base` and `own`) must be identical on 1 thread vs 4
/// threads past `PARALLEL_AXIOMS` when `rooted_merge` is enabled.
#[test]
fn merge_deterministic_allowance_across_threads() {
    let mut body = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SameIndividual(:a0 :a1)\n\
         SubClassOf(:C0 ObjectOneOf(:a2))\n\
         SubClassOf(ObjectOneOf(:a0 :a2) :C0)\n",
    );
    for i in 0..(PARALLEL_AXIOMS + 64) {
        body.push_str(&format!("SubClassOf(:P{i} :Q{i})\n"));
    }
    let input = axioms(&body);

    let mut k1 = Classifier::of(&input);
    let out1 = k1
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .expect("1t classify");
    let (_, base1, own1) = LAST_ALLOWANCE.get();

    let mut k4 = Classifier::of(&input);
    let out4 = k4
        .classify(&Limits {
            threads: 4,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .expect("4t classify");
    let (_, base4, own4) = LAST_ALLOWANCE.get();

    assert_eq!(out1.stats.dl_rooted_merged, 2);
    assert_eq!(out4.stats.dl_rooted_merged, 2);
    assert_eq!(base1, base4, "allowance_base must be thread-independent");
    assert_eq!(
        own1, own4,
        "discretionary allowance must be thread-independent"
    );
}

/// Class-scaled rooted work bound (`SPARKLES_ROOTED_BOUND_PER_CLASS=1`) must be captured
/// and honored across merge restarts instead of hardcoding `ROOTED_WORK_BOUND`.
#[test]
fn merge_restart_honors_class_scaled_bound_and_hub_skip_switch() {
    let mut body = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(:C0 ObjectOneOf(:a1))\n\
         SubClassOf(ObjectOneOf(:a0 :a1) :C0)\n",
    );
    for i in 0..32usize {
        body.push_str(&format!("Declaration(Class(:Extra{i}))\n"));
    }
    let input = axioms(&body);
    with_rooted_bound_per_class(true, || {
        let mut k = Classifier::of(&input);
        let out = k
            .classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge: true,
                ..Limits::default()
            })
            .expect("classify");
        assert!(out.complete);
        assert_eq!(out.stats.dl_rooted_restarts, 1);
        assert_eq!(
            LAST_ROOTED_WORK_LIMIT.get(),
            rooted_work_bound(36, true),
            "captured bound must match class-scaled bound"
        );
    });

    // Hub skipping (`SPARKLES_ROOTED_MERGE_HUB=0`): when the ONLY forced equality involves
    // the generated hub (`⊤ ⊑ {:a}` + `ObjectSomeValuesFrom(owl:topObjectProperty ...)`),
    // hub-merge OFF triggers 0 restarts while hub-merge ON triggers 1 restart, with
    // identical semantics.
    let hub_ax = axioms(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(owl:Thing ObjectOneOf(:a))\n\
         SubClassOf(:Q ObjectSomeValuesFrom(owl:topObjectProperty :A))\n\
         ClassAssertion(:A :a)\n",
    );
    let out_hub_on = with_merge_hub(true, || {
        let mut k = Classifier::of(&hub_ax);
        k.classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .expect("hub on")
    });
    let out_hub_off = with_merge_hub(false, || {
        let mut k = Classifier::of(&hub_ax);
        k.classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .expect("hub off")
    });
    assert!(out_hub_on.complete && out_hub_off.complete);
    assert_eq!(out_hub_on.stats.dl_rooted_restarts, 1);
    assert_eq!(out_hub_off.stats.dl_rooted_restarts, 0);
    assert_eq!(out_hub_on.consistent, out_hub_off.consistent);
}

/// Sol P1 #1 / Neon B1: asserted anonymous individuals (`union_individual`) and harvested
/// anonymous individuals (`union_named`) must hash and look up identically in `Merge`,
/// producing a single component and a flat `CanonMap` even for labels beginning with `_:`.
#[test]
fn merge_canonicalizes_anonymous_keys_across_asserted_and_harvested_paths() {
    let mut m = Merge::default();
    let mut admit = |_: usize| true;
    let limits = Limits::default();

    let bx = Individual::Anonymous(AnonymousIndividual::new("x"));
    let by = Individual::Anonymous(AnonymousIndividual::new("y"));
    assert_eq!(m.union_individual(&bx, &by, 0, &mut admit), Ok(true));
    assert_eq!(m.names.len(), 2);

    // Harvested name `"_:x"` must hit the existing entry rather than inserting a duplicate.
    let nx = NamedIndividual::new("_:x");
    let na = NamedIndividual::new("urn:tierd1291:a");
    assert_eq!(m.union_named(&nx, &na, 0, &mut admit), Ok(true));
    assert_eq!(
        m.names.len(),
        3,
        "harvested _:x must reuse existing anonymous entry"
    );
    // Repeating the same union reports no new progress.
    assert_eq!(m.union_named(&nx, &na, 0, &mut admit), Ok(false));

    // Also cover a blank label that itself starts with `_:`: interner stores `_:_:nested`.
    let bnest = Individual::Anonymous(AnonymousIndividual::new("_:nested"));
    let bnest2 = Individual::Anonymous(AnonymousIndividual::new("_:nested2"));
    assert_eq!(m.union_individual(&bnest, &bnest2, 0, &mut admit), Ok(true));
    let nnest = NamedIndividual::new("_:_:nested");
    let nb = NamedIndividual::new("urn:tierd1291:b");
    assert_eq!(m.union_named(&nnest, &nb, 0, &mut admit), Ok(true));
    assert_eq!(m.names.len(), 6);

    let canon = m
        .build_canon(0, &limits, &mut admit)
        .ok()
        .flatten()
        .expect("non-empty canon");
    assert_eq!(canon.len(), 4);
    assert_eq!(canon.get(&NamedIndividual::new("_:x")), Some(&na));
    assert_eq!(canon.get(&NamedIndividual::new("_:y")), Some(&na));
    assert_eq!(canon.get(&NamedIndividual::new("_:_:nested")), Some(&nb));
    assert_eq!(canon.get(&NamedIndividual::new("_:_:nested2")), Some(&nb));
    // Every representative must itself be a fixed point (flat map).
    for (_, rep) in canon.iter() {
        assert!(
            canon.get(rep).is_none(),
            "CanonMap must be flat: representative {rep:?} is also an alias"
        );
    }
}

/// Sol P2 #3: `collect_same` must release prior Horn `worlds` via `reset()` before growing
/// UF/`CanonMap` and include live `excluded` bytes in its admission base.
#[test]
fn collect_same_releases_prior_horn_worlds_and_admits_excluded() {
    // Build a Horn ontology that populates a substantial `worlds` map (many classes
    // reaching an individual through non-real contexts) plus an excluded disjunctive axiom
    // when run without rooted Tier D.
    let mut body = String::from(
        "SubClassOf(:K ObjectAllValuesFrom(:p :D))\n\
         SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         ClassAssertion(:A :u0)\n",
    );
    for i in 0..160usize {
        body.push_str(&format!(
            "SubClassOf(:W{i} ObjectOneOf(:u0))\n\
             SubClassOf(:W{i} :Tag{i})\n"
        ));
    }
    let base_ax = axioms(&body);
    let mut k = Classifier::of(&base_ax);
    // Run in Horn mode (`rooted: false`) so `k.sat.worlds` and `k.sat.excluded` are both
    // populated on the reused classifier.
    let first = k
        .classify(&Limits {
            threads: 1,
            rooted: false,
            ..Limits::default()
        })
        .expect("initial Horn classify");
    assert!(first.consistent);
    assert!(
        k.sat.worlds.len() >= 100,
        "expected prior Horn run to populate worlds, got {}",
        k.sat.worlds.len()
    );
    assert!(
        !k.sat.excluded.is_empty(),
        "expected prior Horn run to record excluded disjunctive axiom"
    );

    // Add long-IRI SameIndividual axioms and classify with `rooted_merge: true`.
    let rep = NamedIndividual::new(format!("urn:tierd1291:rep_{}", "r".repeat(512)));
    let mut added = Vec::new();
    for i in 0..40usize {
        let alias = NamedIndividual::new(format!("urn:tierd1291:z_{i:02}_{}", "a".repeat(512)));
        added.push(Axiom::new(AxiomKind::SameIndividual(
            [Individual::Named(rep.clone()), Individual::Named(alias)].into(),
        )));
    }
    k.add(&added);

    let (res, sample) = measure(|| {
        k.classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
    });
    let out = res.expect("reused rooted_merge classify");
    assert!(out.complete);
    assert_eq!(out.stats.dl_rooted_merged, 40);
    assert!(
        out.stats.peak_memory >= sample.peak,
        "reused classifier peak_memory ({}) undercounted measured peak ({})",
        out.stats.peak_memory,
        sample.peak
    );
}

/// Neon N3: when optional `collect_same` declines under `max_memory`, the refused
/// unallocated bytes must NOT inflate `stats.peak_memory` above `max_memory` on a
/// subsequent successful classification.
#[test]
fn collect_same_refusal_under_max_memory_does_not_inflate_peak_memory() {
    // 200 distinct long-IRI SameIndividual pairs: during `collect_same::build_canon`,
    // BOTH `Merge` UF and `CanonMap` hold copies of all 400 long IRIs simultaneously, so
    // the prepass peak exceeds the ordinary EL run's peak. Under `cap` between the two,
    // `collect_same` declines and the ordinary run succeeds with `peak_memory <= cap`.
    let mut body = String::from("SubClassOf(:A :B)\n");
    for i in 0..200usize {
        body.push_str(&format!(
            "SameIndividual(:p{i:03}a_{} :p{i:03}b_{})\n",
            "x".repeat(4096),
            "y".repeat(4096)
        ));
    }
    let input = axioms(&body);

    let mut k_off = Classifier::of(&input);
    let off = k_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .expect("merge off");

    let mut saw_declined_ok = false;
    for extra in [0usize, 32 << 10, 64 << 10, 128 << 10, 256 << 10] {
        let cap = off.stats.peak_memory + extra;
        let mut k = Classifier::of(&input);
        if let Ok(out) = k.classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            max_memory: Some(cap),
            ..Limits::default()
        }) && out.stats.dl_rooted_merged == 0
        {
            saw_declined_ok = true;
            assert!(
                out.stats.peak_memory <= cap,
                "refused collect_same inflated peak_memory ({}) above max_memory ({cap})",
                out.stats.peak_memory
            );
        }
    }
    assert!(
        saw_declined_ok,
        "expected at least one cap where collect_same declines and ordinary run succeeds"
    );
}

/// Sol P2 #2 (r2): `excluded_copy_bytes` must perform zero heap allocations and cover
/// retained anonymous blank-node labels, repeated AST payloads, containers, and
/// `Gap::Outside` strings.
#[test]
fn excluded_copy_bytes_is_allocation_free_and_covers_anonymous_and_repeated_ast() {
    let b0 = AnonymousIndividual::new(format!("b0_{}", "a".repeat(4096)));
    let b1 = AnonymousIndividual::new(format!("b1_{}", "b".repeat(4096)));
    let repeated_cls = Class::new(format!("urn:tierd1291:rep_{}", "c".repeat(2048)));
    let repeated_prop = ObjectProperty::new(format!("urn:tierd1291:p_{}", "p".repeat(1024)));
    let repeated_branches: std::collections::BTreeSet<ClassExpression> = (0..8usize)
        .map(|i| {
            let tag = Class::new(format!("urn:tierd1291:t{i}"));
            ClassExpression::ObjectSomeValuesFrom(
                ObjectPropertyExpression::Property(repeated_prop.clone()),
                Box::new(ClassExpression::ObjectIntersectionOf(
                    [
                        ClassExpression::Class(repeated_cls.clone()),
                        ClassExpression::Class(tag),
                    ]
                    .into(),
                )),
            )
        })
        .collect();
    let template = vec![
        Excluded {
            axiom: AxiomKind::DifferentIndividuals(
                [Individual::Anonymous(b0), Individual::Anonymous(b1)].into(),
            ),
            gap: index::Gap::Outside {
                rule: "anonymous-rule".to_owned(),
                message: "x".repeat(512),
            },
        },
        Excluded {
            axiom: AxiomKind::SubClassOf(
                ClassExpression::Class(repeated_cls.clone()),
                ClassExpression::ObjectUnionOf(repeated_branches),
            ),
            gap: index::Gap::Partial("repeated-ast"),
        },
    ];

    // Keep the cloned `Vec<Excluded>` alive through `measure` to observe its true live heap.
    let (cloned, clone_sample) = measure(|| template.clone());
    assert!(
        clone_sample.live > 24 * 1024,
        "expected substantial excluded payload, got {} B",
        clone_sample.live
    );

    let (estimated, est_sample) = measure(|| excluded_copy_bytes(&cloned));
    assert_eq!(
        est_sample.peak, 0,
        "excluded_copy_bytes must not allocate while estimating (allocated {} B)",
        est_sample.peak
    );
    assert!(
        estimated >= clone_sample.live,
        "excluded_copy_bytes ({estimated}) undercounted retained excluded heap ({})",
        clone_sample.live
    );
}

/// Sol P2 #1/#3 & P3 #4 (r2): Item 805 `dl_reduce::plan_budgeted` must pre-admit actual
/// mapped-signature, key-expansion, and retarget payloads without uncharged scratch clones
/// (keeping `Reduction` alive through live-bytes sampling), decline cleanly on post-merge
/// replan `charge` refusal (verified via phase probe, never accepting an error), and
/// cooperatively poll cancellation and deadline expiry inside wide axioms after a merge
/// restart.
#[test]
fn post_merge_replan_admits_long_iri_key_expansion_and_polls_cancellation() {
    use super::admission_test::{MergePhase, PlanKind, with_merge_probe};
    use parking_lot::Mutex;

    // 1. Direct counting-allocator check on `dl_reduce::plan_budgeted` with long IRIs,
    //    keeping `Reduction` alive through `measure` so `sample.live` measures its heap.
    let mut direct_body = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         HasKey(:K (:r) ())\n",
    );
    for i in 0..8usize {
        direct_body.push_str(&format!(
            "ClassAssertion(:K :ind{i:02}_{})\n",
            "l".repeat(1536)
        ));
    }
    let direct_ax = axioms(&direct_body);
    let direct_refs: Vec<&AxiomKind> = direct_ax.iter().map(|a| &a.kind).collect();
    let mut admitted_peak = 0usize;
    let (red_opt, sample) = measure(|| {
        let p = super::dl_reduce::plan_budgeted(
            &direct_refs,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |used| {
                admitted_peak = admitted_peak.max(used);
                true
            },
        )
        .unwrap();
        match p {
            super::dl_reduce::Plan::Ready(red) => Some(red),
            _ => None,
        }
    });
    let red = red_opt.expect("direct plan_budgeted must succeed");
    let retained = red.retained_bytes();
    assert!(
        sample.live > 64 * 1024,
        "expected Reduction live bytes > 64 KiB while alive, got {}",
        sample.live
    );
    assert!(
        admitted_peak >= sample.peak,
        "plan_budgeted admitted peak ({admitted_peak}) undercounted measured peak ({})",
        sample.peak
    );
    assert!(
        retained >= sample.live,
        "Reduction::retained_bytes ({retained}) undercounted measured live ({})",
        sample.live
    );
    drop(red);

    // 1b. Sol P2 #1: wide `DifferentIndividuals` with 200 long IRIs (400 KiB total) must
    //     NOT clone IRIs before admission; a tight cap declines with tiny measured peak.
    let mut diff_inds = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         HasKey(:K (:r) ())\n\
         DifferentIndividuals(",
    );
    for i in 0..200usize {
        diff_inds.push_str(&format!(" :d{i:03}_{}", "x".repeat(2048)));
    }
    diff_inds.push_str(")\n");
    let diff_ax = axioms(&diff_inds);
    let diff_refs: Vec<&AxiomKind> = diff_ax.iter().map(|a| &a.kind).collect();
    let (diff_plan, diff_sample) = measure(|| {
        super::dl_reduce::plan_budgeted(
            &diff_refs,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |used| used <= 2048,
        )
        .unwrap()
    });
    assert!(
        matches!(diff_plan, super::dl_reduce::Plan::Declined),
        "tight cap on wide DifferentIndividuals must decline"
    );
    assert!(
        diff_sample.peak < 32 * 1024,
        "borrowed signature visitor must not clone 400 KiB DifferentIndividuals IRIs before admission (peak={})",
        diff_sample.peak
    );

    // 1c. Sol P2 #1: retargeting `EquivalentObjectProperties` with `owl:topObjectProperty`
    //     and 120 long-IRI roles must pre-admit inclusions before constructing them, and
    //     keep `admitted_peak >= sample.peak` and `retained_bytes >= sample.live`.
    let mut eqop = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         EquivalentObjectProperties(owl:topObjectProperty",
    );
    for i in 0..120usize {
        eqop.push_str(&format!(" :rp{i:03}_{}", "q".repeat(2048)));
    }
    eqop.push_str(")\n");
    let eqop_ax = axioms(&eqop);
    let eqop_refs: Vec<&AxiomKind> = eqop_ax.iter().map(|a| &a.kind).collect();
    let mut eqop_admitted = 0usize;
    let (eqop_red, eqop_sample) = measure(|| {
        match super::dl_reduce::plan_budgeted(
            &eqop_refs,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |used| {
                eqop_admitted = eqop_admitted.max(used);
                true
            },
        )
        .unwrap()
        {
            super::dl_reduce::Plan::Ready(r) => Some(r),
            _ => None,
        }
    });
    let eqop_red = eqop_red.expect("EquivalentObjectProperties retarget must succeed");
    assert!(eqop_sample.live > 200 * 1024);
    assert!(
        eqop_admitted >= eqop_sample.peak,
        "retarget admitted peak ({eqop_admitted}) undercounted measured peak ({})",
        eqop_sample.peak
    );
    assert!(
        eqop_red.retained_bytes() >= eqop_sample.live,
        "retarget retained_bytes ({}) undercounted measured live ({})",
        eqop_red.retained_bytes(),
        eqop_sample.live
    );
    drop(eqop_red);

    let (eqop_refused, eqop_refused_sample) = measure(|| {
        super::dl_reduce::plan_budgeted(
            &eqop_refs,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |used| used <= 16 * 1024,
        )
        .unwrap()
    });
    assert!(matches!(eqop_refused, super::dl_reduce::Plan::Declined));
    assert!(
        eqop_refused_sample.peak <= 32 * 1024,
        "refused EquivalentObjectProperties retarget overshot 16 KiB grant (peak={})",
        eqop_refused_sample.peak
    );

    // 1d. Sol P2 #1: `HasKey` with a complex rewritten `ClassExpression` (using
    //     `owl:topObjectProperty` and long IRIs) and many long-IRI roles must pre-admit
    //     both the rewritten class expression and role uses before construction.
    let mut complex_key = format!(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         ClassAssertion(:A :ck0)\n\
         ClassAssertion(:A :ck1)\n\
         HasKey(ObjectIntersectionOf(:A ObjectSomeValuesFrom(owl:topObjectProperty :LongC_{})) (",
        "c".repeat(4096)
    );
    for i in 0..20usize {
        complex_key.push_str(&format!(" :kr_{i:02}_{}", "r".repeat(1024)));
    }
    complex_key.push_str(") ())\n");
    let ckey_ax = axioms(&complex_key);
    let ckey_refs: Vec<&AxiomKind> = ckey_ax.iter().map(|a| &a.kind).collect();
    let mut ckey_admitted = 0usize;
    let (ckey_red, ckey_sample) = measure(|| {
        match super::dl_reduce::plan_budgeted(
            &ckey_refs,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |used| {
                ckey_admitted = ckey_admitted.max(used);
                true
            },
        )
        .unwrap()
        {
            super::dl_reduce::Plan::Ready(r) => Some(r),
            _ => None,
        }
    });
    let ckey_red = ckey_red.expect("complex HasKey plan must succeed");
    assert!(ckey_sample.live > 32 * 1024);
    assert!(
        ckey_admitted >= ckey_sample.peak,
        "complex HasKey admitted peak ({ckey_admitted}) undercounted measured peak ({})",
        ckey_sample.peak
    );
    assert!(
        ckey_red.retained_bytes() >= ckey_sample.live,
        "complex HasKey retained_bytes ({}) undercounted measured live ({})",
        ckey_red.retained_bytes(),
        ckey_sample.live
    );
    drop(ckey_red);

    // 2. Phase-specific post-merge replan `charge` refusal: Pass 0 succeeds, merges two
    //    large anonymous witnesses (`_:w0 ≈ _:w1`, 512 KiB labels each so `UF + Canon`
    //    exceeds Pass 0's index/context overhead and Pass 1's `plan_budgeted` strictly
    //    exceeds Pass 0's `paid` credit and invokes `charge`), Pass 1's `charge` refuses,
    //    `plan_budgeted` declines, and Horn fallback succeeds (`Ok(out)`, never accepting
    //    an error!).
    let w0 = format!("w0_{}", "a".repeat(512 * 1024));
    let w1 = format!("w1_{}", "b".repeat(512 * 1024));
    let mut body = format!(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(:Z1 :OnlyDisj)\n\
         SubClassOf(:Z2 :OnlyDisj)\n\
         SubClassOf(ObjectOneOf(_:{w0}) :Bridge)\n\
         SubClassOf(:Bridge ObjectOneOf(_:{w1}))\n\
         HasKey(:K (:r) ())\n"
    );
    for i in 0..8usize {
        body.push_str(&format!(
            "ClassAssertion(:K :long{i:02}_{})\n",
            "k".repeat(2048)
        ));
    }
    let input = axioms(&body);

    let mut fresh_horn_k = Classifier::of(&input);
    let fresh_horn = fresh_horn_k
        .classify(&Limits {
            threads: 1,
            rooted: false,
            ..Limits::default()
        })
        .expect("fresh Horn");

    let in_replan = Arc::new(AtomicBool::new(false));
    let refused_in_replan = Arc::new(AtomicBool::new(false));
    let phases: Arc<Mutex<Vec<MergePhase>>> = Arc::new(Mutex::new(Vec::new()));
    let in_replan_probe = in_replan.clone();
    let phases_probe = phases.clone();
    let in_replan_cb = in_replan.clone();
    let refused_cb = refused_in_replan.clone();
    let charge = Arc::new(move |_bytes: usize| -> bool {
        if in_replan_cb.load(Ordering::SeqCst) {
            refused_cb.store(true, Ordering::SeqCst);
            return false;
        }
        true
    });
    let mut k_refuse = Classifier::of(&input);
    let out = with_merge_probe(
        move |phase| {
            match phase {
                MergePhase::PlanStart { restarts: 1 } => {
                    in_replan_probe.store(true, Ordering::SeqCst);
                }
                MergePhase::PlanDone { restarts: 1, .. } => {
                    in_replan_probe.store(false, Ordering::SeqCst);
                }
                _ => {}
            }
            phases_probe.lock().push(phase);
        },
        || {
            k_refuse.classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge: true,
                charge: Some(charge),
                ..Limits::default()
            })
        },
    )
    .expect("post-merge replan charge refusal must fall back cleanly to Horn Ok(out)");
    assert!(
        refused_in_replan.load(Ordering::SeqCst),
        "charge callback must be refused specifically during Pass 1 replan"
    );
    let trace = phases.lock().clone();
    let pos_p0_ready = trace
        .iter()
        .position(|p| {
            matches!(
                p,
                MergePhase::PlanDone {
                    restarts: 0,
                    outcome: PlanKind::Ready
                }
            )
        })
        .expect("Pass 0 plan must succeed");
    let pos_merge_stop = trace
        .iter()
        .position(|p| {
            matches!(
                p,
                MergePhase::SatStop {
                    stop,
                    ..
                } if *stop == super::saturate::STOP_MERGE
            )
        })
        .expect("Pass 0 must stop with STOP_MERGE");
    let pos_restart = trace
        .iter()
        .position(|p| matches!(p, MergePhase::MergeRestart { restarts: 1 }))
        .expect("MergeRestart(1) must occur");
    let pos_p1_declined = trace
        .iter()
        .position(|p| {
            matches!(
                p,
                MergePhase::PlanDone {
                    restarts: 1,
                    outcome: PlanKind::Declined
                }
            )
        })
        .expect("Pass 1 replan must decline on charge refusal");
    assert!(
        pos_p0_ready < pos_merge_stop
            && pos_merge_stop < pos_restart
            && pos_restart < pos_p1_declined,
        "unexpected phase order: {trace:?}"
    );
    assert!(!out.complete);
    assert_eq!(out.stats.dl_rooted_merged, 0);
    assert_eq!(out.consistent, fresh_horn.consistent);
    assert_eq!(k_refuse.excluded(), fresh_horn_k.excluded());

    // 3. Sol P2 #3 & P3 #4: Phase-specific cooperative cancellation and deadline expiry
    //    AFTER the merge restart enters each wide-axiom planner loop.

    // 3a. Cancellation inside wide `DifferentIndividuals` borrowed signature scan on Pass 1.
    let mut wide_diff = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(ObjectOneOf(_:w0) :Bridge)\n\
         SubClassOf(:Bridge ObjectOneOf(_:w1))\n\
         SubClassOf(:Q ObjectSomeValuesFrom(owl:topObjectProperty :Tag))\n\
         DifferentIndividuals(",
    );
    for i in 0..300usize {
        wide_diff.push_str(&format!(" :wi{i:03}"));
    }
    wide_diff.push_str(")\n");
    let wide_diff_ax = axioms(&wide_diff);
    {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_probe = cancel_flag.clone();
        let cancel_cb = cancel_flag.clone();
        let polls_after = Arc::new(AtomicUsize::new(0));
        let polls_after_cb = polls_after.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let mut k = Classifier::of(&wide_diff_ax);
        let err = with_merge_probe(
            move |phase| {
                if matches!(phase, MergePhase::PlanBlockedScan { restarts: 1 }) {
                    cancel_probe.store(true, Ordering::SeqCst);
                }
                trace_probe.lock().push(phase);
            },
            || {
                k.classify(&Limits {
                    threads: 1,
                    rooted: true,
                    rooted_merge: true,
                    cancelled: Some(Arc::new(move || {
                        let armed = cancel_cb.load(Ordering::SeqCst);
                        if armed {
                            polls_after_cb.fetch_add(1, Ordering::SeqCst);
                        }
                        armed
                    })),
                    ..Limits::default()
                })
            },
        )
        .expect_err("must cancel inside wide DifferentIndividuals blocked scan on Pass 1");
        assert_eq!(err, Error::Cancelled);
        assert!(polls_after.load(Ordering::SeqCst) <= 2);
        let t = trace.lock().clone();
        assert!(t.contains(&MergePhase::MergeRestart { restarts: 1 }));
        assert!(t.contains(&MergePhase::PlanBlockedScan { restarts: 1 }));
        assert!(
            !t.contains(&MergePhase::PlanBlockedScanDone { restarts: 1 }),
            "cancellation must stop inside wide DifferentIndividuals scan before it finishes: {t:?}"
        );
        assert!(t.contains(&MergePhase::PlanDone {
            restarts: 1,
            outcome: PlanKind::PollErr
        }));
        // the 300 individuals take 475,505 conclusions without item 1231's repair, and the
        // repair's notices take the run past the rooted bound, so the recovery runs without
        // it and with the check, which item 1327 follows
        let rec = k
            .classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge: true,
                rooted_marker_repair: false,
                rooted_decline: true,
                ..Limits::default()
            })
            .expect("recovery after wide DifferentIndividuals cancel");
        assert!(rec.complete);
        assert_eq!(rec.stats.dl_rooted_restarts, 1);
    }

    // 3b. Cancellation AND deadline expiry inside wide `EquivalentObjectProperties` retarget
    //     expansion on Pass 1.
    let mut wide_eqop = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(ObjectOneOf(_:w0) :Bridge)\n\
         SubClassOf(:Bridge ObjectOneOf(_:w1))\n\
         EquivalentObjectProperties(owl:topObjectProperty",
    );
    for i in 0..300usize {
        wide_eqop.push_str(&format!(" :wr{i:03}"));
    }
    wide_eqop.push_str(")\n");
    let wide_eqop_ax = axioms(&wide_eqop);
    {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_probe = cancel_flag.clone();
        let cancel_cb = cancel_flag.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let mut k = Classifier::of(&wide_eqop_ax);
        let err = with_merge_probe(
            move |phase| {
                if matches!(phase, MergePhase::PlanRetarget { restarts: 1 }) {
                    cancel_probe.store(true, Ordering::SeqCst);
                }
                trace_probe.lock().push(phase);
            },
            || {
                k.classify(&Limits {
                    threads: 1,
                    rooted: true,
                    rooted_merge: true,
                    cancelled: Some(Arc::new(move || cancel_cb.load(Ordering::SeqCst))),
                    ..Limits::default()
                })
            },
        )
        .expect_err("must cancel inside wide EquivalentObjectProperties retarget on Pass 1");
        assert_eq!(err, Error::Cancelled);
        let t = trace.lock().clone();
        assert!(t.contains(&MergePhase::MergeRestart { restarts: 1 }));
        assert!(t.contains(&MergePhase::PlanRetarget { restarts: 1 }));
        assert!(
            !t.contains(&MergePhase::PlanRetargetDone { restarts: 1 }),
            "cancellation must stop inside EquivalentObjectProperties loop before it exits: {t:?}"
        );
        assert!(t.contains(&MergePhase::PlanDone {
            restarts: 1,
            outcome: PlanKind::PollErr
        }));
    }
    {
        // Deadline expiry after Pass 1 enters wide retarget expansion.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(80);
        let slept = Arc::new(AtomicBool::new(false));
        let slept_probe = slept.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let mut k = Classifier::of(&wide_eqop_ax);
        let err = with_merge_probe(
            move |phase| {
                if matches!(phase, MergePhase::PlanRetarget { restarts: 1 })
                    && !slept_probe.swap(true, Ordering::SeqCst)
                {
                    let now = std::time::Instant::now();
                    if deadline > now {
                        std::thread::sleep(deadline - now + std::time::Duration::from_millis(5));
                    }
                }
                trace_probe.lock().push(phase);
            },
            || {
                k.classify(&Limits {
                    threads: 1,
                    rooted: true,
                    rooted_merge: true,
                    deadline: Some(deadline),
                    ..Limits::default()
                })
            },
        )
        .expect_err("must time out inside wide EquivalentObjectProperties retarget on Pass 1");
        assert_eq!(err, Error::Timeout);
        let t = trace.lock().clone();
        assert!(t.contains(&MergePhase::MergeRestart { restarts: 1 }));
        assert!(t.contains(&MergePhase::PlanRetarget { restarts: 1 }));
        assert!(
            !t.contains(&MergePhase::PlanRetargetDone { restarts: 1 }),
            "deadline expiry must stop inside EquivalentObjectProperties loop before it exits: {t:?}"
        );
        assert!(t.contains(&MergePhase::PlanDone {
            restarts: 1,
            outcome: PlanKind::PollErr
        }));
        let rec = k
            .classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge: true,
                ..Limits::default()
            })
            .expect("recovery after wide retarget timeout");
        assert!(rec.complete);
        assert_eq!(rec.stats.dl_rooted_restarts, 1);
    }

    // 3c. Cancellation inside final key-body construction on Pass 1 for a wide `HasKey`
    //     with 200 object properties.
    let mut wide_key = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(ObjectOneOf(_:w0) :Bridge)\n\
         SubClassOf(:Bridge ObjectOneOf(_:w1))\n\
         ClassAssertion(:K :ka)\n\
         ClassAssertion(:K :kb)\n\
         HasKey(:K (",
    );
    for i in 0..200usize {
        wide_key.push_str(&format!(" :kr{i:03}"));
    }
    wide_key.push_str(") ())\n");
    let wide_key_ax = axioms(&wide_key);
    {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_probe = cancel_flag.clone();
        let cancel_cb = cancel_flag.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let mut k = Classifier::of(&wide_key_ax);
        let err = with_merge_probe(
            move |phase| {
                if matches!(phase, MergePhase::PlanKeyBody { restarts: 1 }) {
                    cancel_probe.store(true, Ordering::SeqCst);
                }
                trace_probe.lock().push(phase);
            },
            || {
                k.classify(&Limits {
                    threads: 1,
                    rooted: true,
                    rooted_merge: true,
                    cancelled: Some(Arc::new(move || cancel_cb.load(Ordering::SeqCst))),
                    ..Limits::default()
                })
            },
        )
        .expect_err("must cancel inside wide HasKey final key-body construction on Pass 1");
        assert_eq!(err, Error::Cancelled);
        let t = trace.lock().clone();
        assert!(t.contains(&MergePhase::MergeRestart { restarts: 1 }));
        assert!(t.contains(&MergePhase::PlanKeyBody { restarts: 1 }));
        assert!(
            !t.contains(&MergePhase::PlanKeyBodyDone { restarts: 1 }),
            "cancellation must stop inside final key-body loop before it exits: {t:?}"
        );
        assert!(t.contains(&MergePhase::PlanDone {
            restarts: 1,
            outcome: PlanKind::PollErr
        }));
        let rec = k
            .classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge: true,
                ..Limits::default()
            })
            .expect("recovery after wide key-body cancel");
        assert!(rec.complete);
        assert_eq!(rec.stats.dl_rooted_restarts, 1);
    }

    // 3d. Sol P2 #1 (r3): cancellation inside the streaming Horn check on a wide
    //     `ObjectUnionOf` with zero named individuals before the Horn check loop exits.
    {
        let mut union_members = std::collections::BTreeSet::new();
        for i in 0..300usize {
            union_members.insert(ClassExpression::Class(Class::new(format!(
                "urn:tierd1291:wh{i:03}"
            ))));
        }
        let ax0 = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:tierd1291:A")),
            ClassExpression::ObjectUnionOf(union_members),
        );
        let ax1 = AxiomKind::HasKey(
            ClassExpression::Class(Class::new("urn:tierd1291:K")),
            [ObjectPropertyExpression::Property(ObjectProperty::new(
                "urn:tierd1291:r",
            ))]
            .into(),
            std::collections::BTreeSet::new(),
        );
        let refs = [&ax0, &ax1];
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_probe = cancel_flag.clone();
        let cancel_cb = cancel_flag.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let err = with_merge_probe(
            move |phase| {
                if matches!(phase, MergePhase::PlanHornCheck { restarts: 0 }) {
                    cancel_probe.store(true, Ordering::SeqCst);
                }
                trace_probe.lock().push(phase);
            },
            || {
                super::dl_reduce::plan_budgeted(
                    &refs,
                    None,
                    None,
                    0,
                    &mut || {
                        if cancel_cb.load(Ordering::SeqCst) {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        }
                    },
                    &mut |_| true,
                )
            },
        )
        .err()
        .expect("must cancel inside wide ObjectUnionOf streaming Horn check");
        assert_eq!(err, Error::Cancelled);
        let t = trace.lock().clone();
        assert!(t.contains(&MergePhase::PlanHornCheck { restarts: 0 }));
        assert!(
            !t.contains(&MergePhase::PlanHornCheckDone { restarts: 0 }),
            "cancellation must stop inside streaming Horn check before it exits: {t:?}"
        );
    }

    // 3e. Sol P2 #3 (r3): cancellation inside `Cx::ce_polled` cloning a wide `ObjectOneOf`
    //     of 300 anonymous nominals under `owl:topObjectProperty` before the clone exits.
    {
        let mut anons = std::collections::BTreeSet::new();
        for i in 0..300usize {
            anons.insert(Individual::anonymous(format!("an{i:03}")));
        }
        let disj = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:tierd1291:Z")),
            ClassExpression::ObjectUnionOf(
                [
                    ClassExpression::Class(Class::new("urn:tierd1291:Z1")),
                    ClassExpression::Class(Class::new("urn:tierd1291:Z2")),
                ]
                .into(),
            ),
        );
        let wide_oneof = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:tierd1291:TopOneOf")),
            ClassExpression::ObjectSomeValuesFrom(
                ObjectPropertyExpression::Property(ObjectProperty::top()),
                Box::new(ClassExpression::ObjectOneOf(anons)),
            ),
        );
        let refs = [&disj, &wide_oneof];
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_probe = cancel_flag.clone();
        let cancel_cb = cancel_flag.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let err = with_merge_probe(
            move |phase| {
                if matches!(phase, MergePhase::PlanRetarget { restarts: 0 }) {
                    cancel_probe.store(true, Ordering::SeqCst);
                }
                trace_probe.lock().push(phase);
            },
            || {
                super::dl_reduce::plan_budgeted(
                    &refs,
                    None,
                    None,
                    0,
                    &mut || {
                        if cancel_cb.load(Ordering::SeqCst) {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        }
                    },
                    &mut |_| true,
                )
            },
        )
        .err()
        .expect("must cancel inside wide ObjectOneOf cooperative clone");
        assert_eq!(err, Error::Cancelled);
        let t = trace.lock().clone();
        assert!(t.contains(&MergePhase::PlanRetarget { restarts: 0 }));
        assert!(
            !t.contains(&MergePhase::PlanRetargetDone { restarts: 0 }),
            "cancellation must stop inside ObjectOneOf clone loop before it exits: {t:?}"
        );
    }

    // 3f. Sol P2 #3 (r3): cancellation inside `inds` BTreeSet-to-Vec drain before it exits.
    {
        let mut body = String::from(
            "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
             HasKey(:K (:r) ())\n\
             DifferentIndividuals(",
        );
        for i in 0..300usize {
            body.push_str(&format!(" :di{i:03}"));
        }
        body.push_str(")\n");
        let drain_ax = axioms(&body);
        let refs: Vec<&AxiomKind> = drain_ax.iter().map(|a| &a.kind).collect();
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_probe = cancel_flag.clone();
        let cancel_cb = cancel_flag.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let err = with_merge_probe(
            move |phase| {
                if matches!(phase, MergePhase::PlanIndsDrain { restarts: 0 }) {
                    cancel_probe.store(true, Ordering::SeqCst);
                }
                trace_probe.lock().push(phase);
            },
            || {
                super::dl_reduce::plan_budgeted(
                    &refs,
                    None,
                    None,
                    0,
                    &mut || {
                        if cancel_cb.load(Ordering::SeqCst) {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        }
                    },
                    &mut |_| true,
                )
            },
        )
        .err()
        .expect("must cancel inside named-individual drain loop");
        assert_eq!(err, Error::Cancelled);
        let t = trace.lock().clone();
        assert!(t.contains(&MergePhase::PlanIndsDrain { restarts: 0 }));
        assert!(
            !t.contains(&MergePhase::PlanIndsDrainDone { restarts: 0 }),
            "cancellation must stop inside inds drain loop before it exits: {t:?}"
        );
    }

    // 3g. Sol P2 #3 (r3): `DisjointUnion` collapsing duplicate wide rewritten images reuses
    //     computed accounting without unpolled re-walks and keeps peak/retained sound.
    {
        let mut wide_oneof_a = std::collections::BTreeSet::new();
        for i in 0..24usize {
            wide_oneof_a.insert(Individual::anonymous(format!(
                "du_{i:02}_{}",
                "d".repeat(1024)
            )));
        }
        let wide_filler = ClassExpression::ObjectOneOf(wide_oneof_a);
        let img1 = ClassExpression::ObjectSomeValuesFrom(
            ObjectPropertyExpression::Property(ObjectProperty::top()),
            Box::new(wide_filler.clone()),
        );
        let img2 = ClassExpression::ObjectMinCardinality(
            1,
            ObjectPropertyExpression::Property(ObjectProperty::top()),
            Some(Box::new(wide_filler)),
        );
        let disj = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:tierd1291:Z")),
            ClassExpression::ObjectUnionOf(
                [
                    ClassExpression::Class(Class::new("urn:tierd1291:Z1")),
                    ClassExpression::Class(Class::new("urn:tierd1291:Z2")),
                ]
                .into(),
            ),
        );
        let du =
            AxiomKind::DisjointUnion(Class::new("urn:tierd1291:UnionCls"), [img1, img2].into());
        let refs = [&disj, &du];
        let mut admitted_peak = 0usize;
        let (red_opt, sample) = measure(|| {
            match super::dl_reduce::plan_budgeted(
                &refs,
                None,
                None,
                0,
                &mut || Ok::<(), ()>(()),
                &mut |used| {
                    admitted_peak = admitted_peak.max(used);
                    true
                },
            )
            .unwrap()
            {
                super::dl_reduce::Plan::Ready(r) => Some(r),
                _ => None,
            }
        });
        let red = red_opt.expect("duplicate-image DisjointUnion retarget must succeed");
        assert!(sample.live > 16 * 1024);
        assert!(
            admitted_peak >= sample.peak,
            "DisjointUnion admitted peak ({admitted_peak}) undercounted measured peak ({})",
            sample.peak
        );
        assert!(
            red.retained_bytes() >= sample.live,
            "DisjointUnion retained_bytes ({}) undercounted measured live ({})",
            red.retained_bytes(),
            sample.live
        );
        drop(red);
    }
}

/// Neon N4 / Sol P3 #4 (r2): end-to-end cancellation-vs-`STOP_MERGE` stop-race test
/// explicitly observing both stop winners (`STOP_CANCEL` and `STOP_MERGE`, including
/// `STOP_MERGE` winning followed by cancellation at the restart boundary) and verifying
/// helper thread participation in multi-thread `dl_steps = false` trials.
#[test]
fn merge_stop_vs_cancellation_race_agrees_on_result_and_flags() {
    use super::admission_test::{MergePhase, with_merge_probe};
    use parking_lot::Mutex;

    let mut body = String::from(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         SubClassOf(:C0 ObjectOneOf(:a2))\n\
         SubClassOf(ObjectOneOf(:a0 :a2) :C0)\n\
         ClassAssertion(:Tag :a2)\n\
         SubClassOf(:Probe ObjectSomeValuesFrom(:r ObjectOneOf(:a0)))\n\
         SubClassOf(ObjectSomeValuesFrom(:r :Tag) :Hit)\n",
    );
    // >= EAGER_ROOTS (64 in test) named classes ensure multi-thread runs spawn helpers.
    for i in 0..80usize {
        body.push_str(&format!("SubClassOf(:P{i} :Q{i})\n"));
    }
    let input = axioms(&body);

    // 1. Deterministic 1-thread sweep across every poll point from pre-merge saturation
    //    through the STOP_MERGE restart boundary and Pass 1, observing stop winners.
    let count_polls = Arc::new(AtomicUsize::new(0));
    let cp_cb = count_polls.clone();
    let mut k_probe = Classifier::of(&input);
    let base = k_probe
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            cancelled: Some(Arc::new(move || {
                cp_cb.fetch_add(1, Ordering::SeqCst);
                false
            })),
            ..Limits::default()
        })
        .expect("unrestricted run");
    assert!(base.complete);
    assert_eq!(base.stats.dl_rooted_restarts, 1);
    let total_polls = count_polls.load(Ordering::SeqCst);
    assert!(total_polls >= 4);

    let mut saw_cancel_winner = 0usize;
    let mut saw_merge_winner_then_cancel = 0usize;
    let mut saw_cancelled_dirty = 0usize;
    let mut saw_merge_winner_then_ok = 0usize;
    let expected_instances = vec![
        NamedIndividual::new("urn:tierd1291:a0"),
        NamedIndividual::new("urn:tierd1291:a2"),
    ];
    for cancel_at in 1..=(total_polls + 2) {
        let polls = Arc::new(AtomicUsize::new(0));
        let polls_cb = polls.clone();
        let stops = Arc::new(Mutex::new(Vec::new()));
        let stops_probe = stops.clone();
        let mut k = Classifier::of(&input);
        let res = with_merge_probe(
            move |phase| {
                if let MergePhase::SatStop { stop, .. } = phase {
                    stops_probe.lock().push(stop);
                }
            },
            || {
                k.classify(&Limits {
                    threads: 1,
                    rooted: true,
                    rooted_merge: true,
                    cancelled: Some(Arc::new(move || {
                        polls_cb.fetch_add(1, Ordering::SeqCst) >= cancel_at
                    })),
                    ..Limits::default()
                })
            },
        );
        let sat_stops = stops.lock().clone();
        match res {
            Err(Error::Cancelled) => {
                if sat_stops.contains(&super::saturate::STOP_CANCEL) {
                    saw_cancel_winner += 1;
                }
                if sat_stops.contains(&super::saturate::STOP_MERGE) {
                    saw_merge_winner_then_cancel += 1;
                }
                assert!(
                    !k.sat.dl_worked.load(Ordering::Relaxed),
                    "cancel_at={cancel_at}: cancelled race must not set dl_worked"
                );
                match k.describe(&Class::new("urn:tierd1291:Tag")) {
                    None => saw_cancelled_dirty += 1,
                    Some(d) => {
                        assert_eq!(
                            d.instances, expected_instances,
                            "cancel_at={cancel_at}: post-saturation describe() must be complete"
                        );
                    }
                }
                let after = k
                    .classify(&Limits {
                        threads: 1,
                        rooted: true,
                        rooted_merge: true,
                        ..Limits::default()
                    })
                    .expect("recovery after cancel");
                assert!(after.complete);
                assert_eq!(after.stats.dl_rooted_restarts, 1);
                assert!(after.taxonomy.is_subclass(
                    &Class::new("urn:tierd1291:Probe"),
                    &Class::new("urn:tierd1291:Hit")
                ));
            }
            Ok(out) => {
                if sat_stops.contains(&super::saturate::STOP_MERGE) {
                    saw_merge_winner_then_ok += 1;
                }
                assert!(out.complete);
                assert_eq!(out.stats.dl_rooted_restarts, 1);
                assert!(!out.stats.dl_rooted_over);
                assert!(out.taxonomy.is_subclass(
                    &Class::new("urn:tierd1291:Probe"),
                    &Class::new("urn:tierd1291:Hit")
                ));
            }
            Err(other) => {
                panic!("cancel_at={cancel_at}: unexpected error in stop race: {other:?}")
            }
        }
    }
    assert!(
        saw_cancel_winner >= 1,
        "1-thread sweep must observe STOP_CANCEL winning"
    );
    assert!(
        saw_merge_winner_then_cancel >= 1,
        "1-thread sweep must observe STOP_MERGE winning followed by cancellation"
    );
    assert!(
        saw_cancelled_dirty >= 2 && saw_merge_winner_then_ok >= 1,
        "1-thread sweep must observe dirty cancellation and completed merge restart"
    );

    // 2. Multi-thread (`threads: 4, dl_steps: false`) concurrent cancellation vs STOP_MERGE,
    //    observing helper thread participation and BOTH stop winners (`STOP_CANCEL` and
    //    `STOP_MERGE`, including `STOP_MERGE` followed by restart-boundary cancellation).
    let mt_count_polls = Arc::new(AtomicUsize::new(0));
    let mt_cp_cb = mt_count_polls.clone();
    let mut k_mt_probe = Classifier::of(&input);
    let mt_base = k_mt_probe
        .classify(&Limits {
            threads: 4,
            rooted: true,
            rooted_merge: true,
            dl_steps: false,
            cancelled: Some(Arc::new(move || {
                mt_cp_cb.fetch_add(1, Ordering::SeqCst);
                false
            })),
            ..Limits::default()
        })
        .expect("mt unrestricted run");
    assert!(mt_base.complete);
    assert_eq!(mt_base.stats.dl_rooted_restarts, 1);
    let mt_total_polls = mt_count_polls.load(Ordering::SeqCst);

    let mut saw_mt_cancel_winner_with_helper = false;
    let mut saw_mt_merge_winner_with_helper = false;
    let mut saw_mt_merge_then_cancel_with_helper = false;

    // 2a. Sweep every poll point across the 4-thread `dl_steps: false` run.
    for cancel_at in 1..=(mt_total_polls + 4) {
        let polls = Arc::new(AtomicUsize::new(0));
        let polls_cb = polls.clone();
        let stops = Arc::new(Mutex::new(Vec::new()));
        let stops_probe = stops.clone();
        let mut k = Classifier::of(&input);
        let res = with_merge_probe(
            move |phase| {
                if let MergePhase::SatStop {
                    stop, helper_ran, ..
                } = phase
                {
                    stops_probe.lock().push((stop, helper_ran));
                }
            },
            || {
                k.classify(&Limits {
                    threads: 4,
                    rooted: true,
                    rooted_merge: true,
                    dl_steps: false,
                    cancelled: Some(Arc::new(move || {
                        polls_cb.fetch_add(1, Ordering::SeqCst) >= cancel_at
                    })),
                    ..Limits::default()
                })
            },
        );
        let sat_stops = stops.lock().clone();
        for &(stop, helper_ran) in &sat_stops {
            if stop == super::saturate::STOP_CANCEL && helper_ran {
                saw_mt_cancel_winner_with_helper = true;
            }
            if stop == super::saturate::STOP_MERGE && helper_ran {
                saw_mt_merge_winner_with_helper = true;
            }
        }
        match res {
            Err(Error::Cancelled) => {
                if sat_stops
                    .iter()
                    .any(|&(s, h)| s == super::saturate::STOP_MERGE && h)
                {
                    saw_mt_merge_then_cancel_with_helper = true;
                }
                assert!(!k.sat.dl_worked.load(Ordering::Relaxed));
                let after = k
                    .classify(&Limits {
                        threads: 4,
                        rooted: true,
                        rooted_merge: true,
                        dl_steps: false,
                        ..Limits::default()
                    })
                    .expect("mt recovery");
                assert!(after.complete);
                assert!(after.taxonomy.is_subclass(
                    &Class::new("urn:tierd1291:Probe"),
                    &Class::new("urn:tierd1291:Hit")
                ));
            }
            Ok(out) => {
                assert!(out.complete);
                assert!(!out.stats.dl_rooted_over);
                assert_eq!(out.stats.dl_rooted_restarts, 1);
            }
            Err(other) => panic!("mt cancel_at={cancel_at}: unexpected error {other:?}"),
        }
    }

    // 2b. Deterministic 4-thread `STOP_CANCEL` winner inside `saturate_counted`: arm
    //     cancellation specifically when a saturation worker thread polls `cancelled`.
    {
        let stops = Arc::new(Mutex::new(Vec::new()));
        let stops_probe = stops.clone();
        let mut k = Classifier::of(&input);
        let err = with_merge_probe(
            move |phase| {
                if let MergePhase::SatStop {
                    stop, helper_ran, ..
                } = phase
                {
                    stops_probe.lock().push((stop, helper_ran));
                }
            },
            || {
                k.classify(&Limits {
                    threads: 4,
                    rooted: true,
                    rooted_merge: true,
                    dl_steps: false,
                    cancelled: Some(Arc::new(|| super::saturate::WORKER.get() != 0)),
                    ..Limits::default()
                })
            },
        )
        .expect_err("worker-thread cancellation must stop saturation");
        assert_eq!(err, Error::Cancelled);
        assert!(!k.sat.dl_worked.load(Ordering::Relaxed));
        for &(stop, helper_ran) in stops.lock().iter() {
            if stop == super::saturate::STOP_CANCEL && helper_ran {
                saw_mt_cancel_winner_with_helper = true;
            }
            if stop == super::saturate::STOP_MERGE && helper_ran {
                saw_mt_merge_winner_with_helper = true;
                saw_mt_merge_then_cancel_with_helper = true;
            }
        }
    }

    // 2c. Deterministic 4-thread `STOP_MERGE` winner followed immediately by cancellation
    //     at the restart boundary (`run_merging` poll).
    {
        let cancel_after_merge = Arc::new(AtomicBool::new(false));
        let cam_probe = cancel_after_merge.clone();
        let cam_cb = cancel_after_merge.clone();
        let stops = Arc::new(Mutex::new(Vec::new()));
        let stops_probe = stops.clone();
        let mut k = Classifier::of(&input);
        let err = with_merge_probe(
            move |phase| {
                if let MergePhase::SatStop {
                    stop, helper_ran, ..
                } = phase
                {
                    stops_probe.lock().push((stop, helper_ran));
                    if stop == super::saturate::STOP_MERGE {
                        cam_probe.store(true, Ordering::SeqCst);
                    }
                }
            },
            || {
                k.classify(&Limits {
                    threads: 4,
                    rooted: true,
                    rooted_merge: true,
                    dl_steps: false,
                    cancelled: Some(Arc::new(move || cam_cb.load(Ordering::SeqCst))),
                    ..Limits::default()
                })
            },
        )
        .expect_err("cancellation armed right after STOP_MERGE must cancel at restart");
        assert_eq!(err, Error::Cancelled);
        assert!(!k.sat.dl_worked.load(Ordering::Relaxed));
        let sat_stops = stops.lock().clone();
        assert!(
            sat_stops
                .iter()
                .any(|&(s, h)| s == super::saturate::STOP_MERGE && h),
            "expected STOP_MERGE winner with helper participation before restart cancellation: {sat_stops:?}"
        );
        for &(stop, helper_ran) in &sat_stops {
            if stop == super::saturate::STOP_MERGE && helper_ran {
                saw_mt_merge_winner_with_helper = true;
                saw_mt_merge_then_cancel_with_helper = true;
            }
        }
    }

    assert!(
        saw_mt_cancel_winner_with_helper,
        "4-thread dl_steps=false trials must observe STOP_CANCEL winning on a saturation with helper participation"
    );
    assert!(
        saw_mt_merge_winner_with_helper && saw_mt_merge_then_cancel_with_helper,
        "4-thread dl_steps=false trials must observe STOP_MERGE winning with helper participation and restart cancellation"
    );
}

/// Sol P2 #1 (r3): planner Horn check must not allocate `Vec<Pl>` before admission on a
/// wide class expression followed by an object key with zero named individuals.
#[test]
fn plan_budgeted_wide_union_horn_check_does_not_allocate_before_admission() {
    let mut union_members = std::collections::BTreeSet::new();
    for i in 0..4096usize {
        union_members.insert(ClassExpression::Class(Class::new(format!(
            "urn:tierd1291:wu{i:04}"
        ))));
    }
    let ax0 = AxiomKind::SubClassOf(
        ClassExpression::Class(Class::new("urn:tierd1291:A")),
        ClassExpression::ObjectUnionOf(union_members),
    );
    let ax1 = AxiomKind::HasKey(
        ClassExpression::Class(Class::new("urn:tierd1291:K")),
        [ObjectPropertyExpression::Property(ObjectProperty::new(
            "urn:tierd1291:r",
        ))]
        .into(),
        std::collections::BTreeSet::new(),
    );
    let refs = [&ax0, &ax1];
    let (plan, sample) = measure(|| {
        super::dl_reduce::plan_budgeted(
            &refs,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |_used| false,
        )
        .unwrap()
    });
    assert!(
        matches!(plan, super::dl_reduce::Plan::Declined),
        "0-byte grant must decline wide union + HasKey plan"
    );
    assert_eq!(
        sample.peak, 0,
        "streaming Horn check must not allocate before admission (peak={})",
        sample.peak
    );
}

/// Sol P2 #2 (r3): inserting 12 ordered members and removing the first leaves a 3-node
/// `BTreeSet` of length 11 (5 + 1 + 5) preserved across `clone()`. Both
/// `excluded_copy_bytes` and `dl_reduce::plan_budgeted` (`ObjectOneOf` retarget) must
/// account for the internal root node at length 11 while the clone is live.
#[test]
fn eleven_element_three_node_btree_set_is_not_undercounted_when_cloned() {
    let mut inds = std::collections::BTreeSet::new();
    for i in 0..12usize {
        inds.insert(Individual::named(format!("urn:tierd1291:bt{i:02}")));
    }
    let first = inds.iter().next().unwrap().clone();
    assert!(inds.remove(&first));
    assert_eq!(inds.len(), 11);

    // 1. Retained exclusion cloning (`SameIndividual` with an 11-member 3-node set).
    let excl_template = vec![Excluded {
        axiom: AxiomKind::SameIndividual(inds.clone()),
        gap: index::Gap::Partial("eleven-3node"),
    }];
    let (excl_cloned, excl_clone_sample) = measure(|| excl_template.clone());
    let (excl_est, excl_est_sample) = measure(|| excluded_copy_bytes(&excl_cloned));
    assert_eq!(excl_est_sample.peak, 0);
    assert!(
        excl_est >= excl_clone_sample.live,
        "excluded_copy_bytes ({excl_est}) undercounted live 11-element 3-node BTreeSet ({})",
        excl_clone_sample.live
    );
    drop(excl_cloned);

    // 2. Retargeted `ObjectOneOf` with 11-member 3-node set in `dl_reduce::plan_budgeted`.
    let disj = AxiomKind::SubClassOf(
        ClassExpression::Class(Class::new("urn:tierd1291:Z")),
        ClassExpression::ObjectUnionOf(
            [
                ClassExpression::Class(Class::new("urn:tierd1291:Z1")),
                ClassExpression::Class(Class::new("urn:tierd1291:Z2")),
            ]
            .into(),
        ),
    );
    let top_oneof = AxiomKind::SubClassOf(
        ClassExpression::Class(Class::new("urn:tierd1291:TopSrc")),
        ClassExpression::ObjectSomeValuesFrom(
            ObjectPropertyExpression::Property(ObjectProperty::top()),
            Box::new(ClassExpression::ObjectOneOf(inds)),
        ),
    );
    let refs = [&disj, &top_oneof];
    let mut admitted_peak = 0usize;
    let (red_opt, red_sample) = measure(|| {
        match super::dl_reduce::plan_budgeted(
            &refs,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |used| {
                admitted_peak = admitted_peak.max(used);
                true
            },
        )
        .unwrap()
        {
            super::dl_reduce::Plan::Ready(r) => Some(r),
            _ => None,
        }
    });
    let red = red_opt.expect("retarget of 11-element ObjectOneOf must succeed");
    assert!(
        admitted_peak >= red_sample.peak,
        "plan_budgeted admitted peak ({admitted_peak}) undercounted 11-element 3-node ObjectOneOf peak ({})",
        red_sample.peak
    );
    assert!(
        red.retained_bytes() >= red_sample.live,
        "Reduction::retained_bytes ({}) undercounted 11-element 3-node ObjectOneOf live ({})",
        red.retained_bytes(),
        red_sample.live
    );
    drop(red);
}

/// Sol P3 #5 (r3): `HasKey(ObjectMinCardinality(0 owl:topObjectProperty) (:r) ())` rewrites
/// its key class to `owl:Thing`, so with zero named individuals it emits no fresh
/// equivalence class and succeeds under a tight grant covering only `replaced`.
#[test]
fn has_key_rewritten_to_named_class_reuses_name_without_fresh_equivalence() {
    let input = axioms(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         HasKey(ObjectMinCardinality(0 owl:topObjectProperty) (:r) ())\n",
    );
    let refs: Vec<&AxiomKind> = input.iter().map(|a| &a.kind).collect();
    let plan = super::dl_reduce::plan_budgeted(
        &refs,
        None,
        None,
        0,
        &mut || Ok::<(), ()>(()),
        &mut |used| used <= refs.len(),
    )
    .unwrap();
    let red = match plan {
        super::dl_reduce::Plan::Ready(r) => r,
        other => panic!(
            "tight grant ({}) must accept empty key reduction, got {:?}",
            refs.len(),
            std::mem::discriminant(&other)
        ),
    };
    assert!(
        red.axioms.is_empty(),
        "empty key over ObjectMinCardinality(0 owl:topObjectProperty) must emit no axioms, got {:?}",
        red.axioms
    );
    assert!(red.hub.is_none());

    // With two named individuals, the key uses `owl:Thing` directly without a fresh `k0`
    // equivalence axiom.
    let input2 = axioms(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))\n\
         ClassAssertion(:A :a0)\n\
         ClassAssertion(:A :a1)\n\
         HasKey(ObjectMinCardinality(0 owl:topObjectProperty) (:r) ())\n",
    );
    let refs2: Vec<&AxiomKind> = input2.iter().map(|a| &a.kind).collect();
    let red2 = super::dl_reduce::plan(&refs2, None, None).expect("2-ind key must plan");
    assert!(
        !red2
            .axioms
            .iter()
            .any(|ax| matches!(ax, AxiomKind::EquivalentClasses(_))),
        "key reduced to owl:Thing must not emit a fresh EquivalentClasses axiom: {:?}",
        red2.axioms
    );
}

/// Sol P2 #1 + Neon P2 #1 (r4): singleton `DisjointUnion` rewrite must drop its temporary
/// image container before releasing its budget and allocating `EquivalentClasses`, even
/// when the union class has a long IRI that makes `EquivalentClasses` the peak admission.
#[test]
fn singleton_disjoint_union_with_long_iri_does_not_overlap_live_container_release() {
    let disj = AxiomKind::SubClassOf(
        ClassExpression::Class(Class::new("urn:tierd1291:Z")),
        ClassExpression::ObjectUnionOf(
            [
                ClassExpression::Class(Class::new("urn:tierd1291:Z1")),
                ClassExpression::Class(Class::new("urn:tierd1291:Z2")),
            ]
            .into(),
        ),
    );
    let long_union = Class::new(format!("urn:tierd1291:LongUnion_{}", "u".repeat(4096)));
    let op1 = ClassExpression::ObjectSomeValuesFrom(
        ObjectPropertyExpression::Property(ObjectProperty::top()),
        Box::new(ClassExpression::Class(Class::new("urn:tierd1291:A"))),
    );
    let op2 = ClassExpression::ObjectMinCardinality(
        1,
        ObjectPropertyExpression::Property(ObjectProperty::top()),
        Some(Box::new(ClassExpression::Class(Class::new(
            "urn:tierd1291:A",
        )))),
    );

    // Check both (a) 2-operand DisjointUnion collapsing to 1 image and (b) 1-operand AST.
    for (label, operands, collapsed) in [
        ("2-operand collapsing", vec![op1.clone(), op2], true),
        ("1-operand singleton", vec![op1], false),
    ] {
        let du = AxiomKind::DisjointUnion(long_union.clone(), operands.into_iter().collect());
        let refs = [&disj, &du];

        let mut thresholds = std::collections::BTreeSet::new();
        let _ = super::dl_reduce::plan_budgeted(
            &refs,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |used| {
                thresholds.insert(used);
                true
            },
        )
        .unwrap();

        let mut admitted_peak = 0usize;
        let (red_opt, sample) = measure(|| {
            match super::dl_reduce::plan_budgeted(
                &refs,
                None,
                None,
                0,
                &mut || Ok::<(), ()>(()),
                &mut |used| {
                    admitted_peak = admitted_peak.max(used);
                    true
                },
            )
            .unwrap()
            {
                super::dl_reduce::Plan::Ready(r) => Some(r),
                _ => None,
            }
        });
        let red = red_opt.unwrap_or_else(|| panic!("{label}: unlimited plan must succeed"));
        assert!(
            admitted_peak >= sample.peak,
            "{label}: admitted peak ({admitted_peak}) undercounted measured peak ({})",
            sample.peak
        );
        assert!(
            red.retained_bytes() >= sample.live,
            "{label}: retained_bytes ({}) undercounted measured live ({})",
            red.retained_bytes(),
            sample.live
        );
        if collapsed {
            assert!(red.axioms.iter().any(|ax| matches!(
                ax,
                AxiomKind::SubClassOf(ClassExpression::Class(c), ClassExpression::Class(bottom))
                    if c == &long_union && bottom == &Class::nothing()
            )));
        } else {
            assert!(
                red.axioms
                    .iter()
                    .any(|ax| matches!(ax, AxiomKind::EquivalentClasses(_))),
                "{label}: a single surviving operand must yield an equivalence"
            );
        }
        drop(red);

        // Tight-grant sweep across every observed admission boundary and one byte below it.
        let mut grants = std::collections::BTreeSet::new();
        for &t in &thresholds {
            grants.insert(t);
            if t > 0 {
                grants.insert(t - 1);
            }
        }
        for grant in grants {
            let (plan, tight_sample) = measure(|| {
                super::dl_reduce::plan_budgeted(
                    &refs,
                    None,
                    None,
                    0,
                    &mut || Ok::<(), ()>(()),
                    &mut |used| used <= grant,
                )
                .unwrap()
            });
            assert!(
                tight_sample.peak <= grant,
                "{label}: measured peak ({}) exceeded tight grant ({grant})",
                tight_sample.peak
            );
            if grant == admitted_peak {
                assert!(
                    matches!(plan, super::dl_reduce::Plan::Ready(_)),
                    "{label}: grant equal to admitted_peak ({admitted_peak}) must succeed"
                );
            }
            drop(plan);
        }
    }
}

/// Sol P2 #2 (r4): deduplication in `disjoint_budgeted` must cooperatively poll wide
/// rewritten expressions on both 2nd-operand and 3rd-operand duplicates and cancel
/// mid-comparison before exiting deduplication.
#[test]
fn disjoint_union_deduplication_polls_wide_rewritten_operands_and_cancels() {
    use super::admission_test::{MergePhase, with_merge_probe};
    use parking_lot::Mutex;

    let mut wide_oneof = std::collections::BTreeSet::new();
    for i in 0..300usize {
        wide_oneof.insert(Individual::anonymous(format!(
            "dd_{i:03}_{}",
            "x".repeat(64)
        )));
    }
    let wide_oneof = ClassExpression::ObjectOneOf(wide_oneof);
    let thing_inter = ClassExpression::ObjectIntersectionOf(
        [wide_oneof.clone(), ClassExpression::Class(Class::thing())].into(),
    );
    let op1 = ClassExpression::ObjectSomeValuesFrom(
        ObjectPropertyExpression::Property(ObjectProperty::top()),
        Box::new(thing_inter.clone()),
    );
    let op2 = ClassExpression::ObjectMinCardinality(
        1,
        ObjectPropertyExpression::Property(ObjectProperty::top()),
        Some(Box::new(thing_inter)),
    );
    let op3 = ClassExpression::ObjectSomeValuesFrom(
        ObjectPropertyExpression::Property(ObjectProperty::top()),
        Box::new(ClassExpression::ObjectIntersectionOf(
            [
                wide_oneof,
                ClassExpression::ObjectMinCardinality(
                    0,
                    ObjectPropertyExpression::Property(ObjectProperty::top()),
                    Some(Box::new(ClassExpression::Class(Class::new(
                        "urn:tierd1291:Extra",
                    )))),
                ),
            ]
            .into(),
        )),
    );
    let three_ops: std::collections::BTreeSet<ClassExpression> =
        [op1.clone(), op2.clone(), op3].into();
    assert_eq!(three_ops.len(), 3, "three source operands must be distinct");

    let disj = AxiomKind::SubClassOf(
        ClassExpression::Class(Class::new("urn:tierd1291:Z")),
        ClassExpression::ObjectUnionOf(
            [
                ClassExpression::Class(Class::new("urn:tierd1291:Z1")),
                ClassExpression::Class(Class::new("urn:tierd1291:Z2")),
            ]
            .into(),
        ),
    );
    let union_cls = Class::new("urn:tierd1291:WideDedupUnion");

    // 1. 2-operand cancellation mid-deduplication comparison (former `images.contains(&d)`).
    {
        let du2 = AxiomKind::DisjointUnion(union_cls.clone(), [op1, op2].into());
        let refs = [&disj, &du2];
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_probe = cancel_flag.clone();
        let cancel_cb = cancel_flag.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let err = with_merge_probe(
            move |phase| {
                if matches!(phase, MergePhase::PlanDisjointDedup { restarts: 0 }) {
                    cancel_probe.store(true, Ordering::SeqCst);
                }
                trace_probe.lock().push(phase);
            },
            || {
                super::dl_reduce::plan_budgeted(
                    &refs,
                    None,
                    None,
                    0,
                    &mut || {
                        if cancel_cb.load(Ordering::SeqCst) {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        }
                    },
                    &mut |_| true,
                )
            },
        )
        .err()
        .expect("must cancel mid-deduplication on 2nd duplicate operand");
        assert_eq!(err, Error::Cancelled);
        let t = trace.lock().clone();
        assert!(t.contains(&MergePhase::PlanDisjointDedup { restarts: 0 }));
        assert!(
            !t.contains(&MergePhase::PlanDisjointDedupDone { restarts: 0 }),
            "cancellation must stop inside wide deduplication comparison before exit: {t:?}"
        );
    }

    // 2. 3-operand cancellation mid-deduplication on 3rd operand (former `empty.entry(d)`).
    let du3 = AxiomKind::DisjointUnion(union_cls, three_ops.into());
    let refs3 = [&disj, &du3];
    {
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_probe = cancel_flag.clone();
        let cancel_cb = cancel_flag.clone();
        let dedup_starts = Arc::new(AtomicUsize::new(0));
        let dedup_starts_probe = dedup_starts.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let err = with_merge_probe(
            move |phase| {
                if matches!(phase, MergePhase::PlanDisjointDedup { restarts: 0 })
                    && dedup_starts_probe.fetch_add(1, Ordering::SeqCst) == 1
                {
                    cancel_probe.store(true, Ordering::SeqCst);
                }
                trace_probe.lock().push(phase);
            },
            || {
                super::dl_reduce::plan_budgeted(
                    &refs3,
                    None,
                    None,
                    0,
                    &mut || {
                        if cancel_cb.load(Ordering::SeqCst) {
                            Err(Error::Cancelled)
                        } else {
                            Ok(())
                        }
                    },
                    &mut |_| true,
                )
            },
        )
        .err()
        .expect("must cancel mid-deduplication on 3rd duplicate operand");
        assert_eq!(err, Error::Cancelled);
        let t = trace.lock().clone();
        let starts = t
            .iter()
            .filter(|p| matches!(p, MergePhase::PlanDisjointDedup { restarts: 0 }))
            .count();
        let dones = t
            .iter()
            .filter(|p| matches!(p, MergePhase::PlanDisjointDedupDone { restarts: 0 }))
            .count();
        assert_eq!(
            starts, 2,
            "expected dedup start for 2nd and 3rd operands: {t:?}"
        );
        assert_eq!(dones, 1, "3rd operand dedup must cancel before Done: {t:?}");
    }

    // 3. Uncancelled 3-operand run emits `SubClassOf(d, owl:Nothing)` once and stays sound.
    //    Measure planner allocations without any test probe installed so probe hooks and
    //    trace buffers are excluded from `sample.peak` and `sample.live`.
    {
        let mut admitted_peak = 0usize;
        let (red_opt, sample) = measure(|| {
            match super::dl_reduce::plan_budgeted(
                &refs3,
                None,
                None,
                0,
                &mut || Ok::<(), ()>(()),
                &mut |used| {
                    admitted_peak = admitted_peak.max(used);
                    true
                },
            )
            .unwrap()
            {
                super::dl_reduce::Plan::Ready(r) => Some(r),
                _ => None,
            }
        });
        let red = red_opt.expect("3-operand duplicate-image DisjointUnion must succeed");
        let empty_axioms = red
            .axioms
            .iter()
            .filter(|ax| {
                matches!(
                    ax,
                    AxiomKind::SubClassOf(ClassExpression::ObjectSomeValuesFrom(..), ClassExpression::Class(c))
                        if c.as_str() == crate::vocab::OWL_NOTHING
                )
            })
            .count();
        assert_eq!(
            empty_axioms, 1,
            "3 identical rewritten operands must emit SubClassOf(d, owl:Nothing) exactly once"
        );
        assert!(
            admitted_peak >= sample.peak,
            "3-operand DisjointUnion admitted peak ({admitted_peak}) undercounted peak ({})",
            sample.peak
        );
        assert!(
            red.retained_bytes() >= sample.live,
            "3-operand DisjointUnion retained_bytes ({}) undercounted live ({})",
            red.retained_bytes(),
            sample.live
        );
        drop(red);

        // Verify probe pairing separately outside the counting-allocator sample.
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let plan = with_merge_probe(
            move |phase| trace_probe.lock().push(phase),
            || {
                super::dl_reduce::plan_budgeted(
                    &refs3,
                    None,
                    None,
                    0,
                    &mut || Ok::<(), ()>(()),
                    &mut |_| true,
                )
                .unwrap()
            },
        );
        assert!(matches!(plan, super::dl_reduce::Plan::Ready(_)));
        let t = trace.lock().clone();
        let starts = t
            .iter()
            .filter(|p| matches!(p, MergePhase::PlanDisjointDedup { restarts: 0 }))
            .count();
        let dones = t
            .iter()
            .filter(|p| matches!(p, MergePhase::PlanDisjointDedupDone { restarts: 0 }))
            .count();
        assert_eq!(starts, 2);
        assert_eq!(dones, 2);
    }
}

/// Sol P2 (r5): a duplicate rewritten operand arriving when `images` is at capacity
/// (`len == capacity`, e.g. 1,024 entries) must not reserve unused vector doubling or
/// hash-map growth before deduplication, so a tight grant covering actual allocations
/// still succeeds.
#[test]
fn disjoint_union_full_vector_duplicate_does_not_reserve_unused_growth() {
    let disj = AxiomKind::SubClassOf(
        ClassExpression::Class(Class::new("urn:tierd1291:FullVecZ")),
        ClassExpression::ObjectUnionOf(
            [
                ClassExpression::Class(Class::new("urn:tierd1291:FullVecZ1")),
                ClassExpression::Class(Class::new("urn:tierd1291:FullVecZ2")),
            ]
            .into(),
        ),
    );
    let union_cls = Class::new("urn:tierd1291:FullVecUnion");
    let top = ObjectPropertyExpression::Property(ObjectProperty::top());
    let filler = ClassExpression::Class(Class::new("urn:tierd1291:FullVecA"));

    // Variant order in `ClassExpression`:
    //   ObjectSomeValuesFrom < ObjectHasValue < ObjectMinCardinality
    // So 1 existential + 1,023 distinct `ObjectHasValue` fill `images` to len == cap == 1024
    // before the trailing equivalent `ObjectMinCardinality(1, top, Some(filler))` is visited.
    let first = ClassExpression::ObjectSomeValuesFrom(top.clone(), Box::new(filler.clone()));
    let dup = ClassExpression::ObjectMinCardinality(1, top.clone(), Some(Box::new(filler)));
    let mut base_ops = std::collections::BTreeSet::new();
    base_ops.insert(first);
    for i in 0..1023usize {
        base_ops.insert(ClassExpression::ObjectHasValue(
            top.clone(),
            Individual::anonymous(format!("fv{i}")),
        ));
    }
    assert_eq!(base_ops.len(), 1024);

    let mut dup_ops = base_ops.clone();
    assert!(dup_ops.insert(dup.clone()));
    assert_eq!(dup_ops.len(), 1025);
    assert_eq!(
        dup_ops.last(),
        Some(&dup),
        "duplicate operand must sort last after 1,024 unique operands"
    );

    let du_no_dup = AxiomKind::DisjointUnion(union_cls.clone(), base_ops.into());
    let du_with_dup = AxiomKind::DisjointUnion(union_cls, dup_ops.into());
    let refs_no_dup = [&disj, &du_no_dup];
    let refs_with_dup = [&disj, &du_with_dup];

    // Baseline peak for the 1,024 unique operands alone.
    let mut base_admitted_peak = 0usize;
    let base_plan = super::dl_reduce::plan_budgeted(
        &refs_no_dup,
        None,
        None,
        0,
        &mut || Ok::<(), ()>(()),
        &mut |used| {
            base_admitted_peak = base_admitted_peak.max(used);
            true
        },
    )
    .unwrap();
    assert!(matches!(base_plan, super::dl_reduce::Plan::Ready(_)));
    drop(base_plan);

    // Measure the 1,025-operand (1,024 unique + 1 full-vector duplicate) run.
    let mut admitted_peak = 0usize;
    let (red_opt, sample) = measure(|| {
        match super::dl_reduce::plan_budgeted(
            &refs_with_dup,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |used| {
                admitted_peak = admitted_peak.max(used);
                true
            },
        )
        .unwrap()
        {
            super::dl_reduce::Plan::Ready(r) => Some(r),
            _ => None,
        }
    });
    let red = red_opt.expect("unlimited full-vector duplicate DisjointUnion must succeed");
    assert!(
        admitted_peak >= sample.peak,
        "admitted peak ({admitted_peak}) undercounted measured peak ({})",
        sample.peak
    );
    assert!(
        red.retained_bytes() >= sample.live,
        "retained_bytes ({}) undercounted measured live ({})",
        red.retained_bytes(),
        sample.live
    );
    let empty_axioms = red
        .axioms
        .iter()
        .filter(|ax| {
            matches!(
                ax,
                AxiomKind::SubClassOf(_, ClassExpression::Class(c))
                    if c.as_str() == crate::vocab::OWL_NOTHING
            )
        })
        .count();
    assert_eq!(empty_axioms, 1);
    drop(red);

    // A tight grant covering the 1,024-entry baseline plus the duplicate payload and its
    // emitted `SubClassOf(d, owl:Nothing)` (far below a speculative 2,048-entry `images`
    // doubling) must succeed rather than decline.
    let tight_grant = base_admitted_peak + 4096;
    assert!(
        admitted_peak <= tight_grant,
        "full-vector duplicate over-reserved: admitted_peak={admitted_peak} > tight_grant={tight_grant} (base={base_admitted_peak})"
    );
    let (tight_plan, tight_sample) = measure(|| {
        super::dl_reduce::plan_budgeted(
            &refs_with_dup,
            None,
            None,
            0,
            &mut || Ok::<(), ()>(()),
            &mut |used| used <= tight_grant,
        )
        .unwrap()
    });
    assert!(
        matches!(tight_plan, super::dl_reduce::Plan::Ready(_)),
        "tight grant ({tight_grant}) covering actual allocations must succeed"
    );
    assert!(
        tight_sample.peak <= tight_grant,
        "measured peak ({}) exceeded tight grant ({tight_grant})",
        tight_sample.peak
    );
    drop(tight_plan);

    // Exact boundary check at `admitted_peak` and `admitted_peak - 1`.
    for (grant, expect_ready) in [(admitted_peak, true), (admitted_peak - 1, false)] {
        let (plan, boundary_sample) = measure(|| {
            super::dl_reduce::plan_budgeted(
                &refs_with_dup,
                None,
                None,
                0,
                &mut || Ok::<(), ()>(()),
                &mut |used| used <= grant,
            )
            .unwrap()
        });
        assert_eq!(
            matches!(plan, super::dl_reduce::Plan::Ready(_)),
            expect_ready,
            "grant={grant} expected ready={expect_ready}"
        );
        assert!(
            boundary_sample.peak <= grant,
            "measured peak ({}) exceeded boundary grant ({grant})",
            boundary_sample.peak
        );
        drop(plan);
    }
}

/// Neon P3 (r4): every `Plan*` start probe must pair 1:1 with its `Plan*Done` probe on
/// exact 16-iteration loop boundaries and on `EquivalentObjectProperties` with one
/// non-top property preceding `owl:topObjectProperty`.
#[test]
fn merge_plan_probes_pair_start_and_done_at_exact_boundaries() {
    use super::admission_test::{MergePhase, with_merge_probe};
    use parking_lot::Mutex;

    let count_phase =
        |t: &[MergePhase], target: MergePhase| t.iter().filter(|&&p| p == target).count();
    let assert_all_balanced = |t: &[MergePhase]| {
        for (start, done) in [
            (
                MergePhase::PlanBlockedScan { restarts: 0 },
                MergePhase::PlanBlockedScanDone { restarts: 0 },
            ),
            (
                MergePhase::PlanHornCheck { restarts: 0 },
                MergePhase::PlanHornCheckDone { restarts: 0 },
            ),
            (
                MergePhase::PlanIndsDrain { restarts: 0 },
                MergePhase::PlanIndsDrainDone { restarts: 0 },
            ),
            (
                MergePhase::PlanRetarget { restarts: 0 },
                MergePhase::PlanRetargetDone { restarts: 0 },
            ),
            (
                MergePhase::PlanDisjointDedup { restarts: 0 },
                MergePhase::PlanDisjointDedupDone { restarts: 0 },
            ),
            (
                MergePhase::PlanKeyBody { restarts: 0 },
                MergePhase::PlanKeyBodyDone { restarts: 0 },
            ),
        ] {
            assert_eq!(
                count_phase(t, start),
                count_phase(t, done),
                "unbalanced probe pair {start:?} vs {done:?} in {t:?}"
            );
        }
    };
    let run_trace = |refs: &[&AxiomKind]| -> Vec<MergePhase> {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let trace_probe = trace.clone();
        let plan = with_merge_probe(
            move |phase| trace_probe.lock().push(phase),
            || {
                super::dl_reduce::plan_budgeted(
                    refs,
                    None,
                    None,
                    0,
                    &mut || Ok::<(), ()>(()),
                    &mut |_| true,
                )
                .unwrap()
            },
        );
        assert!(matches!(plan, super::dl_reduce::Plan::Ready(_)));
        let t = trace.lock().clone();
        assert_all_balanced(&t);
        t
    };

    // 1a. Exact 16-iteration boundaries for blocked scan (16 named individuals), Horn check
    //     (14 union members + LHS Class + RHS ObjectUnionOf = 16 ticks), inds drain (16),
    //     and EquivalentObjectProperties with one non-top property preceding owl:topObjectProperty.
    {
        let mut union14 = std::collections::BTreeSet::new();
        for i in 0..14usize {
            union14.insert(ClassExpression::Class(Class::new(format!(
                "urn:tierd1291:b16u{i:02}"
            ))));
        }
        let disj16 = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:tierd1291:B16Z")),
            ClassExpression::ObjectUnionOf(union14),
        );
        let mut diff16 = std::collections::BTreeSet::new();
        for i in 0..16usize {
            diff16.insert(Individual::named(format!("urn:tierd1291:b16i{i:02}")));
        }
        let diff_ax = AxiomKind::DifferentIndividuals(diff16.into());
        // One non-top property sorting before `http://www.w3.org/2002/07/owl#topObjectProperty`.
        let eqop_one_non_top = AxiomKind::EquivalentObjectProperties(
            [
                ObjectPropertyExpression::Property(ObjectProperty::new(
                    "http://example.org/b16_p0",
                )),
                ObjectPropertyExpression::Property(ObjectProperty::top()),
            ]
            .into(),
        );
        let t = run_trace(&[&disj16, &diff_ax, &eqop_one_non_top]);
        assert_eq!(
            count_phase(&t, MergePhase::PlanBlockedScan { restarts: 0 }),
            0
        );
        assert_eq!(
            count_phase(&t, MergePhase::PlanHornCheck { restarts: 0 }),
            0
        );
        assert_eq!(
            count_phase(&t, MergePhase::PlanIndsDrain { restarts: 0 }),
            0
        );
        assert_eq!(count_phase(&t, MergePhase::PlanRetarget { restarts: 0 }), 0);
    }

    // 1b. Exact 16-nominal ObjectOneOf clone and DisjointDedup comparison boundaries.
    {
        let disj = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:tierd1291:B16Z")),
            ClassExpression::ObjectUnionOf(
                [
                    ClassExpression::Class(Class::new("urn:tierd1291:B16Z1")),
                    ClassExpression::Class(Class::new("urn:tierd1291:B16Z2")),
                ]
                .into(),
            ),
        );
        let mut oneof16 = std::collections::BTreeSet::new();
        for i in 0..16usize {
            oneof16.insert(Individual::anonymous(format!("b16a{i:02}")));
        }
        let filler16 = ClassExpression::ObjectOneOf(oneof16);
        let top_oneof16 = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:tierd1291:B16Top")),
            ClassExpression::ObjectSomeValuesFrom(
                ObjectPropertyExpression::Property(ObjectProperty::top()),
                Box::new(filler16.clone()),
            ),
        );
        let t1 = run_trace(&[&disj, &top_oneof16]);
        assert_eq!(
            count_phase(&t1, MergePhase::PlanRetarget { restarts: 0 }),
            0
        );

        let du16 = AxiomKind::DisjointUnion(
            Class::new("urn:tierd1291:B16DU"),
            [
                ClassExpression::ObjectSomeValuesFrom(
                    ObjectPropertyExpression::Property(ObjectProperty::top()),
                    Box::new(filler16.clone()),
                ),
                ClassExpression::ObjectMinCardinality(
                    1,
                    ObjectPropertyExpression::Property(ObjectProperty::top()),
                    Some(Box::new(filler16)),
                ),
            ]
            .into(),
        );
        let t2 = run_trace(&[&disj, &du16]);
        assert_eq!(
            count_phase(&t2, MergePhase::PlanDisjointDedup { restarts: 0 }),
            0
        );
        // Only the outer 2-operand loop in `disjoint_budgeted` fires PlanRetarget (not the
        // 16-nominal `ObjectOneOf` clones).
        assert_eq!(
            count_phase(&t2, MergePhase::PlanRetarget { restarts: 0 }),
            1
        );
    }

    // 2. 17-iteration boundaries emit matching start + done pairs for every probe.
    {
        let mut union15 = std::collections::BTreeSet::new();
        for i in 0..15usize {
            union15.insert(ClassExpression::Class(Class::new(format!(
                "urn:tierd1291:b17u{i:02}"
            ))));
        }
        let disj17 = AxiomKind::SubClassOf(
            ClassExpression::Class(Class::new("urn:tierd1291:B17Z")),
            ClassExpression::ObjectUnionOf(union15),
        );
        let mut diff17 = std::collections::BTreeSet::new();
        for i in 0..17usize {
            diff17.insert(Individual::named(format!("urn:tierd1291:b17i{i:02}")));
        }
        let diff_ax = AxiomKind::DifferentIndividuals(diff17.into());
        let mut oneof17 = std::collections::BTreeSet::new();
        for i in 0..17usize {
            oneof17.insert(Individual::anonymous(format!("b17a{i:02}")));
        }
        let filler17 = ClassExpression::ObjectOneOf(oneof17);
        let du17 = AxiomKind::DisjointUnion(
            Class::new("urn:tierd1291:B17DU"),
            [
                ClassExpression::ObjectSomeValuesFrom(
                    ObjectPropertyExpression::Property(ObjectProperty::top()),
                    Box::new(filler17.clone()),
                ),
                ClassExpression::ObjectMinCardinality(
                    1,
                    ObjectPropertyExpression::Property(ObjectProperty::top()),
                    Some(Box::new(filler17)),
                ),
            ]
            .into(),
        );
        let t = run_trace(&[&disj17, &diff_ax, &du17]);
        assert!(count_phase(&t, MergePhase::PlanBlockedScan { restarts: 0 }) >= 2);
        assert_eq!(
            count_phase(&t, MergePhase::PlanHornCheck { restarts: 0 }),
            1
        );
        assert_eq!(
            count_phase(&t, MergePhase::PlanIndsDrain { restarts: 0 }),
            1
        );
        assert_eq!(
            count_phase(&t, MergePhase::PlanDisjointDedup { restarts: 0 }),
            1
        );
        assert!(count_phase(&t, MergePhase::PlanRetarget { restarts: 0 }) >= 3);
    }
}

/// Design v12 §4.3.5 / r7 #3: strict duplicate-bypass hashbrown admission for `covered`
/// in `taxonomy::build_from_rows`, flat many-rep & wide-parent counting-allocator peak
/// coverage, and large moving-reallocation overlap refusal in `close_rows_admitted`.
#[test]
fn split_taxonomy_hashbrown_duplicate_bypass_wide_peak_and_moving_realloc() {
    use super::dl_clause::{hash_table_bytes, next_hash_table_bytes};
    use super::taxonomy::{MAX_CLASS_COUNT, build_from_rows, close_rows_admitted};
    use rustc_hash::FxHashSet;
    use std::sync::atomic::{AtomicIsize, AtomicUsize};

    // Arithmetic boundary for NOTHING sentinel.
    assert_eq!(MAX_CLASS_COUNT, u32::MAX as usize - 2);

    // 1. Calling-thread counting-allocator check of the exact duplicate-bypass rule on
    //    `FxHashSet<u32>`:
    //    (a) initial 4-bucket table needs 36 B -> 20 B headroom refuses before alloc;
    //    (b) full 3-key table with 0 growth headroom followed by repeated duplicate keys
    //        -> unchanged capacity, 0 allocator events, 0 credit requests!
    //    (c) distinct 4th key requests exact replacement table before alloc;
    //    (d) drop releases exact observed `hash_table_bytes`.
    let need0 = next_hash_table_bytes(0, std::mem::size_of::<u32>()).unwrap();
    assert!(
        need0 > 20,
        "initial 4-bucket table needs > 20 B (got {need0})"
    );
    let mut covered: FxHashSet<u32> = FxHashSet::default();
    let mut held = 0usize;
    let mut credit_reqs = 0usize;
    let try_insert = |set: &mut FxHashSet<u32>,
                      k: u32,
                      cap: usize,
                      held: &mut usize,
                      reqs: &mut usize|
     -> bool {
        if set.contains(&k) {
            return true;
        }
        if set.len() == set.capacity() {
            let Some(need) = next_hash_table_bytes(set.capacity(), std::mem::size_of::<u32>())
            else {
                return false;
            };
            *reqs += 1;
            if *held + need > cap {
                return false;
            }
            let old_live = hash_table_bytes(set.capacity(), std::mem::size_of::<u32>());
            *held += need;
            set.insert(k);
            let new_live = hash_table_bytes(set.capacity(), std::mem::size_of::<u32>());
            *held = held.saturating_sub((old_live + need).saturating_sub(new_live));
        } else {
            set.insert(k);
        }
        true
    };

    // (a) 20 B cap refuses before initial 36 B table allocation.
    let (ok0, s0) = measure(|| try_insert(&mut covered, 1, 20, &mut held, &mut credit_reqs));
    assert!(!ok0);
    assert_eq!(s0.events, 0, "refused initial table must allocate 0 bytes");
    assert_eq!(held, 0);

    // Fill 3 keys into the initial 4-bucket table (capacity == 3).
    assert!(try_insert(
        &mut covered,
        1,
        need0,
        &mut held,
        &mut credit_reqs
    ));
    assert!(try_insert(
        &mut covered,
        2,
        need0,
        &mut held,
        &mut credit_reqs
    ));
    assert!(try_insert(
        &mut covered,
        3,
        need0,
        &mut held,
        &mut credit_reqs
    ));
    assert_eq!(covered.len(), 3);
    assert_eq!(covered.capacity(), 3);
    let cap_at_3 = covered.capacity();
    let reqs_before_dups = credit_reqs;
    let held_at_3 = held;
    assert_eq!(
        held_at_3,
        hash_table_bytes(cap_at_3, std::mem::size_of::<u32>())
    );

    // (b) Zero growth headroom + repeated duplicate inserts: 0 allocs, 0 credit requests!
    let (_, s_dups) = measure(|| {
        for _ in 0..32 {
            assert!(try_insert(
                &mut covered,
                1,
                held_at_3,
                &mut held,
                &mut credit_reqs
            ));
            assert!(try_insert(
                &mut covered,
                2,
                held_at_3,
                &mut held,
                &mut credit_reqs
            ));
            assert!(try_insert(
                &mut covered,
                3,
                held_at_3,
                &mut held,
                &mut credit_reqs
            ));
        }
    });
    assert_eq!(covered.capacity(), cap_at_3);
    assert_eq!(
        s_dups.events, 0,
        "duplicate keys in full set must cause 0 allocator events"
    );
    assert_eq!(
        credit_reqs, reqs_before_dups,
        "duplicate keys must make 0 credit requests"
    );

    // (c) Distinct 4th key refuses under 0 headroom (0 allocs), succeeds with exact replacement!
    let (ok4_ref, s4_ref) =
        measure(|| try_insert(&mut covered, 4, held_at_3, &mut held, &mut credit_reqs));
    assert!(!ok4_ref);
    assert_eq!(s4_ref.events, 0);
    let need4 = next_hash_table_bytes(cap_at_3, std::mem::size_of::<u32>()).unwrap();
    assert!(try_insert(
        &mut covered,
        4,
        held_at_3 + need4,
        &mut held,
        &mut credit_reqs
    ));
    let final_live = hash_table_bytes(covered.capacity(), std::mem::size_of::<u32>());
    assert_eq!(held, final_live);
    drop(covered);
    held -= final_live;
    assert_eq!(held, 0);

    // 2. Large moving-reallocation overlap test in `close_rows_admitted`:
    //    Build a 2-node chain `0 -> 1`, where row 1 has 512 unique subsumers and row 0
    //    has 512 disjoint subsumers. Merging row 1 into row 0 requires `new_cap >= 1024`
    //    (4096 B) while `old_cap` (2048 B) is still live (simultaneous >= 6144 B).
    let n = 1026usize;
    let mut rows: Vec<(bool, Vec<u32>)> = (0..n).map(|i| (false, vec![i as u32])).collect();
    let mut r1: Vec<u32> = (2..514).map(|x| x as u32).collect();
    r1.push(1);
    r1.sort_unstable();
    rows[1] = (false, r1);
    let mut r0: Vec<u32> = (514..1026).map(|x| x as u32).collect();
    r0.push(0);
    r0.push(1);
    r0.sort_unstable();
    rows[0] = (false, r0);

    // First measure unrestricted peak of `close_rows_admitted`.
    let live_u = AtomicIsize::new(0);
    let peak_u = AtomicUsize::new(0);
    let mut rows_u = rows.clone();
    let (res_u, sample_u) = measure(|| {
        close_rows_admitted(&mut rows_u, &[], &|| false, &|d| {
            let now = (live_u.fetch_add(d, Ordering::Relaxed) + d).max(0) as usize;
            peak_u.fetch_max(now, Ordering::Relaxed);
            Ok(())
        })
    });
    assert!(res_u.is_ok());
    assert!(
        peak_u.load(Ordering::Relaxed) >= sample_u.peak,
        "close_rows_admitted admitted peak ({}) must cover measured peak ({})",
        peak_u.load(Ordering::Relaxed),
        sample_u.peak
    );

    // Tight cap strictly below `peak_u` (allowing Tarjan + initial scratch, refusing
    // moving-realloc overlap): must return Halt::Memory cleanly without overshoot.
    let tight = peak_u.load(Ordering::Relaxed) - 1024;
    let live_t = AtomicIsize::new(0);
    let mut rows_t = rows;
    let res_t = close_rows_admitted(&mut rows_t, &[], &|| false, &|d| {
        let cur = live_t.load(Ordering::Relaxed);
        let next = cur + d;
        if d > 0 && next as usize > tight {
            return Err(next as usize);
        }
        live_t.store(next, Ordering::Relaxed);
        Ok(())
    });
    assert!(
        matches!(res_t, Err(super::taxonomy::Halt::Memory(_))),
        "tight cap below moving-realloc overlap must refuse cleanly"
    );
    assert_eq!(
        live_t.load(Ordering::Relaxed),
        0,
        "refusal must release all Tarjan scratch"
    );

    // 3. Flat many-representative (`max_sub = 1`) AND wide-parent (`direct.len() > 16`)
    //    `build_from_rows` calling-thread counting-allocator peak coverage + 2-thread run.
    let build_and_check = |classes: &[Class], subs: &[(bool, Vec<u32>)], threads: usize| {
        let live = AtomicIsize::new(0);
        let peak = AtomicUsize::new(0);
        let (res, sample) = measure(|| {
            build_from_rows(classes, subs, &[], threads, &|| false, &|d| {
                let now = (live.fetch_add(d, Ordering::Relaxed) + d).max(0) as usize;
                peak.fetch_max(now, Ordering::Relaxed);
                Ok(())
            })
        });
        let (tax, owned) = res.expect("build_from_rows must succeed");
        if threads == 1 {
            assert!(
                peak.load(Ordering::Relaxed) >= sample.peak,
                "build_from_rows 1t admitted peak ({}) < measured peak ({})",
                peak.load(Ordering::Relaxed),
                sample.peak
            );
        }
        assert_eq!(live.load(Ordering::Relaxed) as usize, owned);
        assert_eq!(tax.nodes().len(), classes.len() + 2);
    };

    // Flat 128 classes (`max_sub = 1`).
    let flat_cls: Vec<Class> = (0..128)
        .map(|i| Class::new(format!("urn:flat:C{i:03}")))
        .collect();
    let flat_subs: Vec<(bool, Vec<u32>)> = (0..128).map(|i| (false, vec![i as u32])).collect();
    build_and_check(&flat_cls, &flat_subs, 1);
    build_and_check(&flat_cls, &flat_subs, 2);

    // Wide-parent hierarchy: bottom class `C0` has 40 incomparable direct parents `C1..C40`
    // (`> GREEDY_MAX = 16`), forcing `covered` wide-row compaction + duplicate ancestors!
    let wide_n = 48usize;
    let wide_cls: Vec<Class> = (0..wide_n)
        .map(|i| Class::new(format!("urn:wide:C{i:03}")))
        .collect();
    let mut wide_subs: Vec<(bool, Vec<u32>)> =
        (0..wide_n).map(|i| (false, vec![i as u32])).collect();
    // C1..C40 each subsume shared ancestors C41..C47 so `covered` sees many duplicates!
    for row in wide_subs.iter_mut().take(41).skip(1) {
        for anc in 41..wide_n {
            row.1.push(anc as u32);
        }
    }
    wide_subs[0].1 = (0..wide_n as u32).collect();
    build_and_check(&wide_cls, &wide_subs, 1);
    build_and_check(&wide_cls, &wide_subs, 2);
}

/// Design v12 §4.3.5 + §4.3.4: single-ledger counting-allocator coverage on `shoi-042`
/// multi-leaf split, index-free `retained_bytes` (zero panic with `index == None`),
/// follow-up recovery after memory refusal, between-rounds `test_admitted` with `partial`
/// false/true, and single-call incremental dynamic-fold exact total (`140`, not `150`).
#[test]
fn split_single_ledger_retained_bytes_between_rounds_and_dynamic_fold() {
    use super::dl_rules::test_admitted;
    use super::saturate::Dynamic;

    // 1. Multi-leaf split on shoi-042: verify index-free `retained_bytes` never panics,
    //    admitted peak covers single-threaded counting-allocator heap, and follow-up call
    //    recovers cleanly after a tight memory refusal.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
    let doc = crate::ofn_read::parse(&std::fs::read_to_string(dir.join("shoi-042.ofn")).unwrap())
        .unwrap();
    let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
    let lim_split = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let (out, sample) = measure(|| sat.classify(&refs, None, &lim_split).unwrap());
    assert!(out.complete);
    assert!(out.stats.dl_rooted_splits >= 1);
    assert!(out.stats.dl_rooted_leaves >= 2);
    assert!(
        sat.index.is_none() && sat.split_answer.is_some(),
        "forked run must retain SplitAnswer without index"
    );
    let ret = sat.retained_bytes(0, 0, 0);
    assert!(
        ret > 0,
        "index-free retained_bytes must succeed without panicking"
    );
    assert!(
        out.stats.peak_memory >= sample.peak,
        "split peak_memory ({}) must cover measured heap peak ({})",
        out.stats.peak_memory,
        sample.peak
    );

    // Tight cap mid-split -> Tier D declines cleanly to Horn fallback (`dl_over_budget`),
    // clearing stale `split_answer`, and preserving `peak_memory`.
    let mid_lim = Limits {
        max_memory: Some(out.stats.peak_memory / 2),
        ..lim_split.clone()
    };
    let fb = sat.classify(&refs, None, &mid_lim).unwrap();
    assert!(!fb.complete && fb.stats.dl_over_budget);
    assert!(sat.split_answer.is_none());

    // Ultra-tight cap below Horn too -> returns `Err(Error::Memory)`, invalidates prior
    // `split_answer` (`describe == None`), and a follow-up unrestricted call recovers.
    let _ = sat.classify(&refs, None, &lim_split).unwrap();
    assert!(sat.split_answer.is_some());
    let tiny_lim = Limits {
        max_memory: Some(1024),
        ..lim_split.clone()
    };
    let err = sat.classify(&refs, None, &tiny_lim).unwrap_err();
    assert!(matches!(err, Error::Memory { .. }), "got {err:?}");
    assert!(sat.split_answer.is_none());
    assert!(
        sat.describe(&Class::new("http://example.org/d#C0"))
            .is_none()
    );
    let recovered = sat.classify(&refs, None, &lim_split).unwrap();
    assert_eq!(recovered.taxonomy, out.taxonomy);

    // 2. Between-rounds `test_admitted` at a `Dynamic` segment boundary:
    //    refuses without allocating segment or publishing ID.
    let mut d = Dynamic::new(100);
    for i in 0..1024u32 {
        let _ = d.intern(&[i, i + 5000]);
    }
    d.sync_unadmitted_on_begin_run();
    let len_before = d.len();
    struct CapDynBudget {
        live: usize,
        cap: usize,
        overflowed: bool,
    }
    impl super::saturate::DynamicBudget for CapDynBudget {
        fn admit(&mut self, bytes: usize) -> bool {
            let Some(next) = self.live.checked_add(bytes) else {
                self.overflowed = true;
                return false;
            };
            if next > self.cap {
                return false;
            }
            self.live = next;
            true
        }
        fn credit(&mut self, bytes: usize) {
            self.live = self.live.saturating_sub(bytes);
        }
        fn on_overflow(&mut self) {
            self.overflowed = true;
        }
    }
    let mut b_ref = CapDynBudget {
        live: 100,
        cap: 200,
        overflowed: false,
    };
    let (r_ref, s_ref) = measure(|| test_admitted(&d, 1, &[2, 3], &mut b_ref));
    assert_eq!(r_ref, None);
    assert_eq!(
        s_ref.events, 0,
        "refused test_admitted at segment boundary must allocate 0 B"
    );
    assert_eq!(d.len(), len_before);
    assert_eq!(b_ref.live, 100);

    // 3. Single-call incremental dynamic-fold lifetime check (design §4.3.4 / r10 #1):
    //    base=100, pre-existing dynamic payload=30 rebased on begin_run, round 1 admits
    //    new bytes -> unadmitted_bytes stays equal to pre_bytes, final fold adds ONLY
    //    `unadmitted_bytes` to `totals.memory` -> exact `100 + d.bytes()` (no double count!).
    let mut d_inc = Dynamic::new(100);
    let _ = d_inc.intern(&[1, 2]);
    let pre_bytes = d_inc.bytes();
    assert!(pre_bytes > 0);
    d_inc.sync_unadmitted_on_begin_run();
    assert_eq!(d_inc.unadmitted_bytes(), pre_bytes);
    let mut b_ok = CapDynBudget {
        live: 100 + pre_bytes,
        cap: 100 + pre_bytes + 4096,
        overflowed: false,
    };
    let id_new = test_admitted(&d_inc, 3, &[4], &mut b_ok).expect("new test in existing segment");
    assert!(id_new >= 100);
    let added = d_inc.bytes() - pre_bytes;
    assert!(added > 0);
    assert_eq!(
        d_inc.unadmitted_bytes(),
        pre_bytes,
        "admitted insertion must NOT increase unadmitted_bytes"
    );
    let totals_mem_after_r1 = 100 + added;
    let final_fold = totals_mem_after_r1 + d_inc.unadmitted_bytes();
    assert_eq!(
        final_fold,
        100 + d_inc.bytes(),
        "final dynamic fold must equal base + full dynamic bytes exactly once (no double count)"
    );
}

/// Sol r1 #1, #2, #3 + Neon r1 B2:
/// 1. Production-path leaf-export overlap refusal: cap allows leaf saturation and export
///    scratch individually, but excludes their overlap (`leaf_live + export_scratch`),
///    refusing inside `export_split_leaf` and recording the combined peak.
/// 2. Allocation-site refusal tests with counting allocator: original-signature collection
///    refusal (before un-admitted growth) and parallel `taxonomy::build_inconsistent`
///    refusal when cap allows `bottom` alone but excludes `bottom + chunks`.
/// 3. Nested forks + forced merges (`STOP_MERGE`) with live sibling `Arc<CanonMap>`s on
///    `stack` under both a tight hard cap and a refusing pool callback.
#[test]
fn split_leaf_export_overlap_allocation_sites_nested_forks_and_parallel_inconsistent() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // 1. Parallel `taxonomy::build_inconsistent` (`threads > 1 && classes.len() >= MIN_CLASSES`):
    //    cap allows `bottom` (`bytes`) alone, excludes `bottom + chunks` -> refuses cleanly
    //    BEFORE allocating `chunks`, then full cap succeeds and settles exact live debit!
    {
        let n = super::MIN_CLASSES + 64;
        let classes: Vec<Class> = (0..n)
            .map(|i| Class::new(format!("urn:incons:C{i:04}")))
            .collect();
        let iri_bytes: usize = classes.iter().map(|c| c.as_str().len()).sum();
        let bottom_bytes = 2 * std::mem::size_of::<super::taxonomy::Node>()
            + (n + 2) * std::mem::size_of::<Class>()
            + n * std::mem::size_of::<(u32, u32)>()
            + iri_bytes
            + crate::vocab::OWL_NOTHING.len()
            + crate::vocab::OWL_THING.len();
        // Cap strictly between `bottom_bytes` and `bottom_bytes + chunks_bytes`!
        let overlap_cap = bottom_bytes + 64;
        let live = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let refused = super::taxonomy::build_inconsistent(&classes, 4, &|| false, &|d: isize| {
            if d >= 0 {
                let next = live.load(Ordering::Relaxed).saturating_add(d as usize);
                peak.fetch_max(next, Ordering::Relaxed);
                if next > overlap_cap {
                    return Err(next);
                }
                live.store(next, Ordering::Relaxed);
            } else {
                live.fetch_sub(d.unsigned_abs(), Ordering::Relaxed);
            }
            Ok(())
        });
        assert!(
            matches!(refused, Err(super::taxonomy::Halt::Memory(used)) if used > overlap_cap),
            "parallel build_inconsistent must refuse before allocating overlapping chunks"
        );
        assert_eq!(
            live.load(Ordering::Relaxed),
            0,
            "refused parallel build_inconsistent must release bottom debit cleanly"
        );

        // Full cap succeeds, records peak > bottom_bytes (covering `bottom + chunks`),
        // and drops `chunks` debit back to exact `owned` bytes.
        let live_ok = AtomicUsize::new(0);
        let peak_ok = AtomicUsize::new(0);
        let (tax, owned) =
            super::taxonomy::build_inconsistent(&classes, 4, &|| false, &|d: isize| {
                if d >= 0 {
                    let next = live_ok.load(Ordering::Relaxed).saturating_add(d as usize);
                    peak_ok.fetch_max(next, Ordering::Relaxed);
                    live_ok.store(next, Ordering::Relaxed);
                } else {
                    live_ok.fetch_sub(d.unsigned_abs(), Ordering::Relaxed);
                }
                Ok(())
            })
            .expect("parallel build_inconsistent must succeed under full cap");
        assert_eq!(owned, bottom_bytes);
        assert_eq!(live_ok.load(Ordering::Relaxed), owned);
        assert!(
            peak_ok.load(Ordering::Relaxed) >= owned + n * std::mem::size_of::<Class>(),
            "parallel build_inconsistent peak ({}) must cover bottom + chunks overlap",
            peak_ok.load(Ordering::Relaxed)
        );
        assert_eq!(tax.nodes().len(), 2);
    }

    // 2. Original-signature collection pre-admission refusal (Sol r1 #2):
    //    with many classes and a tiny memory cap, `run_splitting` refuses during
    //    `raw_classes`/`raw_inds` growth BEFORE un-admitted growth and reports attempted peak.
    {
        let mut ax = Vec::new();
        let a = crate::model::Individual::named("http://example.org/sig#a");
        let b = crate::model::Individual::named("http://example.org/sig#b");
        ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
            crate::model::ClassExpression::ObjectOneOf(std::collections::BTreeSet::from([
                a.clone(),
                b.clone(),
            ])),
            a,
        )));
        for i in 0..256 {
            ax.push(crate::model::Axiom::new(AxiomKind::Declaration(
                crate::model::Entity::Class(Class::new(format!("http://example.org/sig#C{i:03}"))),
            )));
        }
        let refs: Vec<&AxiomKind> = ax.iter().map(|x| &x.kind).collect();
        let cap = 256usize;
        let mut sat = Saturation::new();
        let lim = Limits {
            threads: 1,
            rooted: true,
            rooted_eq_split: true,
            max_memory: Some(cap),
            ..Limits::default()
        };
        let (res, sample) = measure(|| sat.classify(&refs, None, &lim));
        assert!(
            matches!(res, Err(Error::Memory { used, .. }) if used > cap),
            "tiny cap must refuse original-signature collection cleanly, got {res:?}"
        );
        assert!(
            sample.peak < 8192,
            "refused signature collection must stop before full table allocation (peak={})",
            sample.peak
        );
    }

    // 3. Nested-frame pool refusal with derived `MergeRestart` (Sol r2 P2 #2 + Sol r3 P2 #2):
    //    Two independent binary splits over 16 KiB individual IRIs using pure-EL singleton
    //    nominals (so `scope_bytes` stays free of long IRIs) plus a pure-EL conditional
    //    merge (`a == b => :BothAB(:a) => :Trigger(:x) => x == y` inside `B_eq_1`) with
    //    `probe = 0`. Depth-1 `B_eq` triggers a real `MergeRestart`, then depth-2 `SplitFork`
    //    carries live sibling frames and distinct `Arc<CanonMap>`s whose child `Union`/`Canon`
    //    admissions strictly exceed every earlier watermark. Arm pool refusal strictly AFTER
    //    both `MergeRestart` and `SplitFork { splits >= 2, depth >= 2 }` and assert the exact
    //    nested `SplitAlloc { site, proposed, admitted: false }` event plus clean recovery.
    {
        use super::admission_test::{MergePhase, SplitAllocSite, with_merge_probe};
        let pad = "p".repeat(16 * 1024);
        let ofn = format!(
            "Prefix(:=<http://example.org/nest#>)\n\
             Ontology(<http://example.org/nest>\n\
               SubClassOf(:X1 ObjectUnionOf(:B1 :C1))\n\
               SubClassOf(:B1 ObjectOneOf(:b_{pad}))\n\
               SubClassOf(:C1 ObjectOneOf(:c_{pad}))\n\
               ClassAssertion(:X1 :a_{pad})\n\
               SubClassOf(:X2 ObjectUnionOf(:E2 :F2))\n\
               SubClassOf(:E2 ObjectOneOf(:e_{pad}))\n\
               SubClassOf(:F2 ObjectOneOf(:f_{pad}))\n\
               ClassAssertion(:X2 :d_{pad})\n\
               ClassAssertion(:IsA :a_{pad})\n\
               ClassAssertion(:IsB :b_{pad})\n\
               SubClassOf(ObjectIntersectionOf(:IsA :IsB) :BothAB)\n\
               ObjectPropertyAssertion(:inv :x_{pad} :a_{pad})\n\
               SubClassOf(ObjectSomeValuesFrom(:inv :BothAB) :Trigger)\n\
               SubClassOf(:Trigger ObjectOneOf(:y_{pad}))\n\
               ClassAssertion(:P :b_{pad})\n\
               ClassAssertion(:P :c_{pad})\n\
               ClassAssertion(:Q :e_{pad})\n\
               ClassAssertion(:Q :f_{pad})\n\
               SubClassOf(:A ObjectOneOf(:a_{pad}))\n\
               SubClassOf(:D ObjectOneOf(:d_{pad}))\n\
             )"
        );
        let doc = crate::ofn_read::parse(&ofn).unwrap();
        let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let lim_base = Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            ..Limits::default()
        };
        let mut sat = Saturation::new();
        let ok = super::with_split_knobs(Some(0), Some(true), Some(4), Some(16), || {
            sat.classify(&refs, None, &lim_base).unwrap()
        });
        assert!(
            ok.complete
                && ok.stats.dl_rooted_splits >= 2
                && ok.stats.dl_rooted_leaves >= 3
                && ok.stats.dl_rooted_restarts >= 1,
            "nested fixture must exercise both splits>=2 and derived merge restarts>=1: {:?}",
            ok.stats
        );

        let saw_merge_restart = Arc::new(AtomicBool::new(false));
        let saw_nested_fork = Arc::new(AtomicBool::new(false));
        let armed = Arc::new(AtomicBool::new(false));
        let pool_refused = Arc::new(AtomicBool::new(false));
        let refused_sites = Arc::new(parking_lot::Mutex::new(
            Vec::<(SplitAllocSite, usize)>::new(),
        ));
        let armed_cb = armed.clone();
        let pr = pool_refused.clone();
        let lim_pool = Limits {
            charge: Some(Arc::new(move |_more: usize| {
                if armed_cb.load(Ordering::Acquire) {
                    pr.store(true, Ordering::Release);
                    return false;
                }
                true
            })),
            ..lim_base.clone()
        };
        let mut sat_pool = Saturation::new();
        let smr = saw_merge_restart.clone();
        let sf = saw_nested_fork.clone();
        let arm_probe = armed.clone();
        let rs = refused_sites.clone();
        let pool_out = with_merge_probe(
            move |phase| match phase {
                MergePhase::MergeRestart { restarts } if restarts >= 1 => {
                    smr.store(true, Ordering::Release);
                }
                MergePhase::SplitFork { splits, depth, .. } if splits >= 2 && depth >= 2 => {
                    sf.store(true, Ordering::Release);
                    arm_probe.store(true, Ordering::Release);
                }
                MergePhase::SplitAlloc {
                    site,
                    proposed,
                    admitted: false,
                } => {
                    rs.lock().push((site, proposed));
                }
                _ => {}
            },
            || {
                super::with_split_knobs(Some(0), Some(true), Some(4), Some(16), || {
                    sat_pool.classify(&refs, None, &lim_pool)
                })
            },
        );
        assert!(
            saw_merge_restart.load(Ordering::Acquire),
            "nested-frame fixture must observe a real MergeRestart before nested refusal"
        );
        assert!(
            saw_nested_fork.load(Ordering::Acquire),
            "nested-frame fixture must reach splits>=2, depth>=2 before refusing"
        );
        assert!(
            pool_refused.load(Ordering::Acquire),
            "armed nested-frame pool callback must have refused"
        );
        let ref_sites = refused_sites.lock().clone();
        assert!(
            ref_sites.iter().any(|&(site, proposed)| matches!(
                site,
                SplitAllocSite::ForkStack
                    | SplitAllocSite::ForkMergeClone
                    | SplitAllocSite::ForkNeq
                    | SplitAllocSite::ForkEqUnion
                    | SplitAllocSite::ForkEqCanon
            ) && proposed > 64 * 1024),
            "nested refusal must anchor to a nested fork SplitAlloc refusal, got {ref_sites:?}"
        );
        match pool_out {
            Ok(fb) => {
                assert!(!fb.complete && fb.stats.dl_over_budget);
                assert!(sat_pool.split_answer.is_none());
            }
            Err(Error::Memory { .. }) => {
                assert!(sat_pool.split_answer.is_none());
            }
            Err(other) => panic!("unexpected error on nested-frame pool refusal: {other:?}"),
        }
        let rec = super::with_split_knobs(Some(0), Some(true), Some(4), Some(16), || {
            sat_pool.classify(&refs, None, &lim_base).unwrap()
        });
        assert_eq!(rec.taxonomy, ok.taxonomy);
    }

    // 4. Dedicated leaf-export overlap refusal test (Sol r1 #1, anchored per Sol r2 P2 #2):
    //    Build 200 named aliases `i000..i199` all asserted `SameIndividual` with `:rep` plus
    //    200 classes `C000..C199` (`SubClassOf(:Root :Ck)`, `ClassAssertion(:Root :rep)`)
    //    beside `a = b ∨ a = c` (`probe = 0` forces an immediate fork). During saturation,
    //    canonicalization collapses all 200 aliases to `:rep` so saturation is tiny, while
    //    `export_split_leaf` projects back to the full 200-individual × 200-class original
    //    signature (`leaf_insts` > 160 KiB) while `leaf_live` is still held! Inside the SAME
    //    split run, record the exact pool watermark at `SplitLeafExportStart` and
    //    `SplitLeafExportDone`, set `overlap_cap` strictly between them, and assert
    //    `SplitFork`, `SplitLeafExportStart`, AND `SplitLeafExportRefused` all fire.
    {
        use super::admission_test::{MergePhase, with_merge_probe};
        let n_classes = 200usize;
        let n_aliases = 200usize;
        let mut ax = Vec::with_capacity(n_classes + n_aliases + 8);
        let a = crate::model::Individual::named("http://example.org/ov#a");
        let b = crate::model::Individual::named("http://example.org/ov#b");
        let c_ind = crate::model::Individual::named("http://example.org/ov#c");
        let rep = crate::model::Individual::named("http://example.org/ov#rep");
        let root_cls = Class::new("http://example.org/ov#Root");
        ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
            crate::model::ClassExpression::ObjectOneOf(std::collections::BTreeSet::from([
                b, c_ind,
            ])),
            a,
        )));
        ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
            crate::model::ClassExpression::Class(root_cls.clone()),
            rep.clone(),
        )));
        for i in 0..n_classes {
            let ci = Class::new(format!("http://example.org/ov#C{i:03}"));
            ax.push(crate::model::Axiom::sub_class_of(root_cls.clone(), ci));
        }
        for i in 0..n_aliases {
            let alias = crate::model::Individual::named(format!("http://example.org/ov#i{i:03}"));
            ax.push(crate::model::Axiom::new(AxiomKind::SameIndividual(
                std::collections::BTreeSet::from([rep.clone(), alias]),
            )));
        }
        let refs: Vec<&AxiomKind> = ax.iter().map(|x| &x.kind).collect();
        let lim_fork = Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            ..Limits::default()
        };

        let charged = Arc::new(AtomicUsize::new(0));
        let pre_export_wm = Arc::new(AtomicUsize::new(0));
        let post_export_wm = Arc::new(AtomicUsize::new(0));
        let ch_cb = charged.clone();
        let ch_probe = charged.clone();
        let pre_p = pre_export_wm.clone();
        let post_p = post_export_wm.clone();
        let mut sat_fork = Saturation::new();
        let fork_res = with_merge_probe(
            move |phase| match phase {
                MergePhase::SplitLeafExportStart { leaves: 0 } => {
                    pre_p.store(ch_probe.load(Ordering::Acquire), Ordering::Release);
                }
                MergePhase::SplitLeafExportDone { leaves: 0 } => {
                    post_p.store(ch_probe.load(Ordering::Acquire), Ordering::Release);
                }
                _ => {}
            },
            || {
                super::with_split_knobs(Some(0), Some(true), Some(2), Some(4), || {
                    sat_fork
                        .classify(
                            &refs,
                            None,
                            &Limits {
                                charge: Some(Arc::new(move |more: usize| {
                                    ch_cb.fetch_add(more, Ordering::AcqRel);
                                    true
                                })),
                                ..lim_fork.clone()
                            },
                        )
                        .unwrap()
                })
            },
        );
        assert!(fork_res.complete && fork_res.stats.dl_rooted_splits >= 1);
        let pre_wm = pre_export_wm.load(Ordering::Acquire);
        let post_wm = post_export_wm.load(Ordering::Acquire);
        assert!(
            post_wm > pre_wm + 1024,
            "first leaf export watermark ({post_wm}) must strictly exceed pre-export leaf watermark ({pre_wm})"
        );

        let overlap_cap = pre_wm + (post_wm - pre_wm) / 2;
        let saw_fork = Arc::new(AtomicBool::new(false));
        let saw_export_start = Arc::new(AtomicBool::new(false));
        let saw_export_refused = Arc::new(AtomicBool::new(false));
        let sf = saw_fork.clone();
        let ses = saw_export_start.clone();
        let ser = saw_export_refused.clone();
        let mut sat_ov = Saturation::new();
        let ov_res = with_merge_probe(
            move |phase| match phase {
                MergePhase::SplitFork { .. } => sf.store(true, Ordering::Release),
                MergePhase::SplitLeafExportStart { .. } => ses.store(true, Ordering::Release),
                MergePhase::SplitLeafExportRefused => ser.store(true, Ordering::Release),
                _ => {}
            },
            || {
                super::with_split_knobs(Some(0), Some(true), Some(2), Some(4), || {
                    sat_ov.classify(
                        &refs,
                        None,
                        &Limits {
                            max_memory: Some(overlap_cap),
                            ..lim_fork.clone()
                        },
                    )
                })
            },
        );
        assert!(saw_fork.load(Ordering::Acquire), "must fork before export");
        assert!(
            saw_export_start.load(Ordering::Acquire),
            "leaf saturation must succeed under overlap_cap={overlap_cap} (pre_wm={pre_wm})"
        );
        assert!(
            saw_export_refused.load(Ordering::Acquire),
            "refusal must occur specifically inside export_split_leaf under overlap_cap={overlap_cap} (post_wm={post_wm})"
        );
        match ov_res {
            Ok(fb) => {
                assert!(!fb.complete && fb.stats.dl_over_budget);
                assert!(sat_ov.split_answer.is_none());
            }
            Err(Error::Memory { used, .. }) => {
                assert!(used > overlap_cap);
                assert!(sat_ov.split_answer.is_none());
            }
            Err(other) => panic!("unexpected error on leaf-export overlap refusal: {other:?}"),
        }
    }
}

/// Round-2 + Round-3 regression suite covering:
/// 1. Neon B1 + Sol r3 P2 #2c: monotonic `SplitLedger` watermark & accepted deltas around
///    `ledger.add`/`remap_neq` -> `ledger.cover` in real `SplitFork` and `MergeRestart`.
/// 2. Sol r2 P1 #1a + Sol r3 P2 #2b: asserted `SameIndividual` UF + signature overlap refusal
///    anchored to `SplitAlloc { site: OrigSig, admitted: false }`.
/// 3. Sol r3 P1 #2: reused `Saturation` with LONG individual names (no asserted `SameIndividual`)
///    AND forked leaf export overlap with long individual names, verifying `interner.individuals`
///    storage is counted in both entry baseline and `leaf_live`.
/// 4. Sol r2 P1 #2 + Sol r3 P1 #2: post-leaf reset and `SplitAnswer` publication drop
///    `canon_want`, free `leaf_individuals`, and leave `interner.bytes() == 0` and
///    `residual_outer_bytes() == 0`.
/// 5. Sol r2 P1 #3 + Sol r3 P2 #2b: site-anchored `RootStack` refusal AND separate
///    `SynthAxioms` (`DifferentIndividuals` `BTreeSet`) admission refusal.
/// 6. Sol r3 P1 #1: long-IRI (2 × 64 KiB) `STOP_SPLIT` handoff refusal with counting-allocator
///    assertion proving zero unbudgeted IRI clones occur before admission.
#[test]
fn split_round2_single_ledger_entry_baseline_and_cleanup_regressions() {
    use super::admission_test::{LedgerChargeSource, MergePhase, SplitAllocSite, with_merge_probe};

    // 1. Neon B1 + Sol r3 P2 #2c: capture every `LedgerCharge` watermark and accepted delta
    //    around real `Delta -> Cover` sequences inside both `SplitFork` and `MergeRestart`
    //    on `shoi-042`, proving monotonic watermark progression even at intermediate levels
    //    strictly below the final peak!
    {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
        let doc =
            crate::ofn_read::parse(&std::fs::read_to_string(dir.join("shoi-042.ofn")).unwrap())
                .unwrap();
        let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let total_charged = Arc::new(AtomicUsize::new(0));
        let tc = total_charged.clone();
        let phases = Arc::new(parking_lot::Mutex::new(Vec::<MergePhase>::new()));
        let ph_cb = phases.clone();
        let mut sat = Saturation::new();
        let out = with_merge_probe(
            move |p| ph_cb.lock().push(p),
            || {
                sat.classify(
                    &refs,
                    None,
                    &Limits {
                        threads: 1,
                        rooted: true,
                        rooted_merge: true,
                        rooted_eq_split: true,
                        charge: Some(Arc::new(move |more: usize| {
                            tc.fetch_add(more, Ordering::AcqRel);
                            true
                        })),
                        ..Limits::default()
                    },
                )
                .unwrap()
            },
        );
        assert!(
            out.complete && out.stats.dl_rooted_splits >= 2 && out.stats.dl_rooted_restarts >= 1
        );
        let recorded = phases.lock().clone();
        let mut prev_paid_after = 0usize;
        let mut in_fork = false;
        let mut in_merge = false;
        let mut last_source: Option<LedgerChargeSource> = None;
        let mut saw_fork_delta_to_cover = false;
        let mut saw_merge_delta_to_cover = false;
        let mut ledger_deltas_sum = 0usize;
        for p in &recorded {
            match *p {
                MergePhase::SplitFork { .. } => {
                    in_fork = true;
                    in_merge = false;
                    last_source = None;
                }
                MergePhase::MergeRestart { .. } => {
                    in_merge = true;
                    in_fork = false;
                    last_source = None;
                }
                MergePhase::SatStop { .. } => {
                    in_fork = false;
                    in_merge = false;
                    last_source = None;
                }
                MergePhase::LedgerCharge {
                    source,
                    used,
                    paid_before,
                    paid_after,
                    requested,
                    admitted,
                } => {
                    assert!(admitted, "unbounded shoi-042 charge must succeed");
                    assert!(
                        paid_after >= paid_before,
                        "watermark must not decrease within one charge: before={paid_before}, after={paid_after}"
                    );
                    assert_eq!(
                        requested,
                        used.saturating_sub(paid_before),
                        "each ledger {source:?} call must request precisely the amount above the current watermark (used={used}, paid_before={paid_before}, requested={requested})"
                    );
                    assert_eq!(
                        paid_after - paid_before,
                        requested,
                        "watermark advance must equal real charge callback request"
                    );
                    assert!(
                        paid_before >= prev_paid_after,
                        "watermark regressed across consecutive charges (stale paid cache!): prev_after={prev_paid_after}, cur_before={paid_before}, source={source:?}, used={used}"
                    );
                    if last_source == Some(LedgerChargeSource::Delta)
                        && source == LedgerChargeSource::Cover
                    {
                        if in_fork {
                            saw_fork_delta_to_cover = true;
                        }
                        if in_merge {
                            saw_merge_delta_to_cover = true;
                        }
                    }
                    prev_paid_after = paid_after;
                    last_source = Some(source);
                    ledger_deltas_sum = ledger_deltas_sum.saturating_add(requested);
                }
                _ => {}
            }
        }
        assert!(
            saw_fork_delta_to_cover,
            "shoi-042 must exercise Delta -> Cover inside a real SplitFork"
        );
        assert!(
            saw_merge_delta_to_cover,
            "shoi-042 must exercise Delta -> Cover inside a real MergeRestart"
        );
        let charged = total_charged.load(Ordering::Acquire);
        assert!(
            charged <= out.stats.peak_memory && ledger_deltas_sum <= charged,
            "cumulative pool charges ({charged}, ledger_deltas={ledger_deltas_sum}) must never exceed peak_memory ({})",
            out.stats.peak_memory
        );
        assert_eq!(
            sat.interner.bytes(),
            0,
            "completed SplitAnswer must leave zero replacement interner bytes"
        );
        assert_eq!(
            sat.residual_outer_bytes(),
            0,
            "completed SplitAnswer must leave zero residual outer bytes"
        );
        assert!(
            out.stats.memory <= charged,
            "final reported memory ({}) must not exceed admitted pool watermark ({charged})",
            out.stats.memory
        );
        // Exact-peak pool budget must complete without spurious refusal.
        let exact_cap = out.stats.peak_memory;
        let pool_used = Arc::new(AtomicUsize::new(0));
        let pu = pool_used.clone();
        let mut sat_exact = Saturation::new();
        let exact_out = sat_exact
            .classify(
                &refs,
                None,
                &Limits {
                    threads: 1,
                    rooted: true,
                    rooted_merge: true,
                    rooted_eq_split: true,
                    charge: Some(Arc::new(move |more: usize| {
                        let next = pu.fetch_add(more, Ordering::AcqRel) + more;
                        next <= exact_cap
                    })),
                    ..Limits::default()
                },
            )
            .unwrap();
        assert!(
            exact_out.complete,
            "pool capped at exact peak_memory ({exact_cap}) must succeed without double-charge refusal"
        );
        assert_eq!(exact_out.taxonomy, out.taxonomy);
    }

    // 2. Sol r2 P1 #1a + Sol r3 P2 #2b: Asserted `SameIndividual` UF + signature overlap refusal,
    //    anchored specifically to `SplitAlloc { site: OrigSig, admitted: false }`.
    {
        let mut ax = Vec::new();
        let pad = "x".repeat(256);
        for i in 0..80usize {
            let u = crate::model::Individual::named(format!("http://example.org/sa#u{i:03}_{pad}"));
            let v = crate::model::Individual::named(format!("http://example.org/sa#v{i:03}_{pad}"));
            ax.push(crate::model::Axiom::new(AxiomKind::SameIndividual(
                std::collections::BTreeSet::from([u, v]),
            )));
        }
        let refs: Vec<&AxiomKind> = ax.iter().map(|x| &x.kind).collect();
        let mut sat_probe = Saturation::new();
        let admitted_probe = Arc::new(AtomicUsize::new(0));
        let mut peak_probe = 0usize;
        let lim_unlim = Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            ..Limits::default()
        };
        sat_probe
            .collect_same(&refs, 0, &lim_unlim, &admitted_probe, &mut peak_probe)
            .unwrap();
        let prepass_live = sat_probe
            .merge
            .uf_bytes()
            .saturating_add(sat_probe.canon_live_bytes());
        assert!(
            prepass_live > 32 * 1024,
            "expected substantial prepass UF+canon: {prepass_live}"
        );

        let cap_overlap = prepass_live.saturating_add(16 * 1024);
        let mut sat_cap = Saturation::new();
        let admitted_cap = Arc::new(AtomicUsize::new(0));
        let mut peak_cap = 0usize;
        sat_cap
            .collect_same(&refs, 0, &lim_unlim, &admitted_cap, &mut peak_cap)
            .unwrap();
        let lim_cap = Limits {
            max_memory: Some(cap_overlap),
            ..lim_unlim.clone()
        };
        let cap_orig_sig = Arc::new(parking_lot::Mutex::new(None::<(usize, bool)>));
        let cos = cap_orig_sig.clone();
        let err_cap = with_merge_probe(
            move |p| {
                if let MergePhase::SplitAlloc {
                    site: SplitAllocSite::OrigSig,
                    proposed,
                    admitted,
                } = p
                {
                    *cos.lock() = Some((proposed, admitted));
                }
            },
            || {
                sat_cap
                    .run_splitting(
                        &refs,
                        None,
                        0,
                        &lim_cap,
                        &admitted_cap,
                        &mut peak_cap,
                        true,
                        AttemptProvenance::Ineligible,
                    )
                    .unwrap_err()
            },
        );
        assert!(
            matches!(err_cap, Error::Memory { used, .. } if used > cap_overlap),
            "signature admission must include live asserted-SameIndividual UF+canon ({prepass_live}) against cap {cap_overlap}, got {err_cap:?}"
        );
        let cap_site = *cap_orig_sig.lock();
        assert!(
            matches!(cap_site, Some((proposed, false)) if proposed > cap_overlap),
            "refusal must occur specifically at OrigSig with proposed > {cap_overlap}, got {cap_site:?}"
        );

        // Same overlap refusal through a `Limits::charge` pool, anchored to OrigSig!
        let mut sat_pool = Saturation::new();
        let admitted_pool = Arc::new(AtomicUsize::new(0));
        let mut peak_pool = 0usize;
        sat_pool
            .collect_same(&refs, 0, &lim_unlim, &admitted_pool, &mut peak_pool)
            .unwrap();
        admitted_pool.store(prepass_live, Ordering::Relaxed);
        let pool_rem = Arc::new(AtomicUsize::new(16 * 1024));
        let pr = pool_rem.clone();
        let lim_pool = Limits {
            charge: Some(Arc::new(move |more: usize| {
                let cur = pr.load(Ordering::Acquire);
                if more > cur {
                    return false;
                }
                pr.fetch_sub(more, Ordering::AcqRel);
                true
            })),
            ..lim_unlim
        };
        let pool_orig_sig = Arc::new(parking_lot::Mutex::new(None::<(usize, bool)>));
        let pos = pool_orig_sig.clone();
        let err_pool = with_merge_probe(
            move |p| {
                if let MergePhase::SplitAlloc {
                    site: SplitAllocSite::OrigSig,
                    proposed,
                    admitted,
                } = p
                {
                    *pos.lock() = Some((proposed, admitted));
                }
            },
            || {
                sat_pool
                    .run_splitting(
                        &refs,
                        None,
                        0,
                        &lim_pool,
                        &admitted_pool,
                        &mut peak_pool,
                        true,
                        AttemptProvenance::Ineligible,
                    )
                    .unwrap_err()
            },
        );
        assert!(
            matches!(err_pool, Error::Memory { used, .. } if used > cap_overlap),
            "pool signature admission must include live prepass UF+canon, got {err_pool:?}"
        );
        let pool_site = *pool_orig_sig.lock();
        assert!(
            matches!(pool_site, Some((proposed, false)) if proposed > cap_overlap),
            "pool refusal must occur specifically at OrigSig with proposed > {cap_overlap}, got {pool_site:?}"
        );
    }

    // 3. Sol r3 P1 #2: Reused `Saturation` with LONG named individual IRIs (NO asserted
    //    `SameIndividual`, so `collect_same` returns early without resetting) + forked leaf
    //    export overlap with LONG named individual IRIs.
    {
        // 3a. Reused saturation with 100 individuals carrying 2 KiB IRIs (~200 KiB in
        //     `sat.interner.individuals`) and tiny class count.
        let mut ind_ax = Vec::new();
        let pad = "i".repeat(2048);
        let cls_c = Class::new("http://example.org/indreuse#C");
        for i in 0..100usize {
            let ind = crate::model::Individual::named(format!(
                "http://example.org/indreuse#x{i:03}_{pad}"
            ));
            ind_ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
                crate::model::ClassExpression::Class(cls_c.clone()),
                ind,
            )));
        }
        let ind_refs: Vec<&AxiomKind> = ind_ax.iter().map(|x| &x.kind).collect();
        let mut sat = Saturation::new();
        let first = sat
            .classify(
                &ind_refs,
                None,
                &Limits {
                    threads: 1,
                    ..Limits::default()
                },
            )
            .unwrap();
        assert!(first.complete && sat.index.is_some());
        let inds_storage = sat
            .interner
            .individuals
            .bytes()
            .saturating_add(sat.interner.individuals.name_bytes());
        assert!(
            inds_storage > 200 * 1024,
            "expected >200 KiB in interner.individuals: {inds_storage}"
        );
        let entry_live = sat.leaf_live_bytes_without_canon_and_merge();
        assert!(
            entry_live > inds_storage,
            "entry baseline ({entry_live}) must include interner.individuals storage ({inds_storage})"
        );

        // Second ontology has NO `SameIndividual` (so `collect_same` returns early without
        // resetting `sat`) and a modest ~6 KiB signature.
        let mut small_ax = Vec::new();
        let a = crate::model::Individual::named("http://example.org/reuse2#a");
        let b = crate::model::Individual::named("http://example.org/reuse2#b");
        small_ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
            crate::model::ClassExpression::ObjectOneOf(std::collections::BTreeSet::from([
                a.clone(),
                b,
            ])),
            a,
        )));
        for i in 0..40usize {
            small_ax.push(crate::model::Axiom::new(AxiomKind::Declaration(
                crate::model::Entity::Class(Class::new(format!(
                    "http://example.org/reuse2#S{i:02}_{}",
                    "y".repeat(64)
                ))),
            )));
        }
        let small_refs: Vec<&AxiomKind> = small_ax.iter().map(|x| &x.kind).collect();
        let cap_reuse = entry_live.saturating_add(3072);
        let lim_reuse = Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            max_memory: Some(cap_reuse),
            ..Limits::default()
        };
        let mut sat_fresh = Saturation::new();
        let fresh_ok = sat_fresh.classify(&small_refs, None, &lim_reuse).unwrap();
        assert!(
            fresh_ok.complete,
            "fresh saturation must fit easily under cap_reuse={cap_reuse}"
        );

        let reuse_orig_sig = Arc::new(parking_lot::Mutex::new(None::<(usize, bool)>));
        let ros = reuse_orig_sig.clone();
        let reuse_err = with_merge_probe(
            move |p| {
                if let MergePhase::SplitAlloc {
                    site: SplitAllocSite::OrigSig,
                    proposed,
                    admitted,
                } = p
                {
                    *ros.lock() = Some((proposed, admitted));
                }
            },
            || sat.classify(&small_refs, None, &lim_reuse).unwrap_err(),
        );
        assert!(
            matches!(reuse_err, Error::Memory { used, .. } if used > cap_reuse),
            "reused saturation signature admission must count live interner.individuals ({inds_storage}) in entry_live ({entry_live}) against cap {cap_reuse}, got {reuse_err:?}"
        );
        let ros_val = *reuse_orig_sig.lock();
        assert!(
            matches!(ros_val, Some((proposed, false)) if proposed > cap_reuse),
            "reused long-individual refusal must occur at OrigSig with proposed > {cap_reuse}, got {ros_val:?}"
        );

        // 3b. Forked leaf export overlap with LONG individual names:
        //     10 unmerged individuals with 32 KiB IRIs (`ClassAssertion(:Tag :x_i)`) keep
        //     >320 KiB live in `sat.interner.individuals` with minimal saturation overhead,
        //     while 300 classes × 400 aliased instances keep saturation small and expand
        //     `leaf_insts` (~480 KiB) past `pre_w` so `export_split_leaf` must refuse under
        //     `lf_cap` ONLY when `leaf_live` counts those >320 KiB of `interner.individuals`!
        let mut lf_ax = Vec::new();
        let pad32k = "j".repeat(32 * 1024);
        let a_lf = crate::model::Individual::named("http://example.org/lf#a");
        let b_lf = crate::model::Individual::named("http://example.org/lf#b");
        let c_lf = crate::model::Individual::named("http://example.org/lf#c");
        let rep_lf = crate::model::Individual::named("http://example.org/lf#rep");
        let root_lf = Class::new("http://example.org/lf#Root");
        let tag_lf = Class::new("http://example.org/lf#Tag");
        lf_ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
            crate::model::ClassExpression::ObjectOneOf(std::collections::BTreeSet::from([
                b_lf.clone(),
                c_lf.clone(),
            ])),
            a_lf.clone(),
        )));
        lf_ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
            crate::model::ClassExpression::Class(root_lf.clone()),
            rep_lf.clone(),
        )));
        for i in 0..300usize {
            let ci = Class::new(format!("http://example.org/lf#C{i:03}"));
            lf_ax.push(crate::model::Axiom::sub_class_of(root_lf.clone(), ci));
        }
        for i in 0..400usize {
            let alias = crate::model::Individual::named(format!("http://example.org/lf#i{i:03}"));
            lf_ax.push(crate::model::Axiom::new(AxiomKind::SameIndividual(
                std::collections::BTreeSet::from([rep_lf.clone(), alias]),
            )));
        }
        for i in 0..10usize {
            let ind =
                crate::model::Individual::named(format!("http://example.org/lf#x{i:02}_{pad32k}"));
            lf_ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
                crate::model::ClassExpression::Class(tag_lf.clone()),
                ind,
            )));
        }
        let lf_refs: Vec<&AxiomKind> = lf_ax.iter().map(|x| &x.kind).collect();
        let lim_lf = Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            ..Limits::default()
        };
        let leaf_live_proposed = Arc::new(AtomicUsize::new(0));
        let pre_exp_wm = Arc::new(AtomicUsize::new(0));
        let post_exp_wm = Arc::new(AtomicUsize::new(0));
        let ch_tot = Arc::new(AtomicUsize::new(0));
        let llp = leaf_live_proposed.clone();
        let pre_p = pre_exp_wm.clone();
        let post_p = post_exp_wm.clone();
        let ch_probe = ch_tot.clone();
        let ch_cb = ch_tot.clone();
        let mut sat_lf = Saturation::new();
        let lf_ok = with_merge_probe(
            move |phase| match phase {
                MergePhase::SplitAlloc {
                    site: SplitAllocSite::LeafLive,
                    proposed,
                    admitted: true,
                } if llp.load(Ordering::Acquire) == 0 => {
                    llp.store(proposed, Ordering::Release);
                }
                MergePhase::SplitLeafExportStart { leaves: 0 } => {
                    pre_p.store(ch_probe.load(Ordering::Acquire), Ordering::Release);
                }
                MergePhase::SplitLeafExportDone { leaves: 0 } => {
                    post_p.store(ch_probe.load(Ordering::Acquire), Ordering::Release);
                }
                _ => {}
            },
            || {
                super::with_split_knobs(Some(0), Some(true), Some(2), Some(4), || {
                    sat_lf
                        .classify(
                            &lf_refs,
                            None,
                            &Limits {
                                charge: Some(Arc::new(move |more: usize| {
                                    ch_cb.fetch_add(more, Ordering::AcqRel);
                                    true
                                })),
                                ..lim_lf.clone()
                            },
                        )
                        .unwrap()
                })
            },
        );
        assert!(lf_ok.complete && lf_ok.stats.dl_rooted_splits >= 1);
        let ll_prop = leaf_live_proposed.load(Ordering::Acquire);
        // `orig_sig` holds 2 copies (~640 KiB) and the saturated leaf's `interner.individuals`
        // holds another ~320 KiB simultaneously (`> 960 KiB` total at `LeafLive`).
        assert!(
            ll_prop > 960 * 1024,
            "LeafLive proposed total ({ll_prop}) must include both orig_sig and live interner.individuals (>960 KiB)"
        );
        let pre_w = pre_exp_wm.load(Ordering::Acquire);
        let post_w = post_exp_wm.load(Ordering::Acquire);
        assert!(
            post_w > pre_w + 512,
            "long-individual leaf export watermark ({post_w}) must exceed pre-export ({pre_w})"
        );
        let lf_cap = pre_w + (post_w - pre_w) / 2;
        let saw_lf_ref = Arc::new(AtomicBool::new(false));
        let slr = saw_lf_ref.clone();
        let mut sat_lf_cap = Saturation::new();
        let lf_cap_res = with_merge_probe(
            move |p| {
                if matches!(p, MergePhase::SplitLeafExportRefused) {
                    slr.store(true, Ordering::Release);
                }
            },
            || {
                super::with_split_knobs(Some(0), Some(true), Some(2), Some(4), || {
                    sat_lf_cap.classify(
                        &lf_refs,
                        None,
                        &Limits {
                            max_memory: Some(lf_cap),
                            ..lim_lf
                        },
                    )
                })
            },
        );
        assert!(
            saw_lf_ref.load(Ordering::Acquire),
            "long-individual leaf export must refuse under overlap cap {lf_cap}"
        );
        match lf_cap_res {
            Ok(fb) => assert!(!fb.complete && fb.stats.dl_over_budget),
            Err(Error::Memory { used, .. }) => assert!(used > lf_cap),
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    // 4. Sol r2 P1 #2 + Sol r3 P1 #2: Leaf cleanup drops `canon_want`, frees `leaf_individuals`,
    //    and leaves `interner.bytes() == 0` and `residual_outer_bytes() == 0` at every leaf
    //    reset and after `SplitAnswer` publication.
    {
        let ofn = "Prefix(:=<http://example.org/lc#>)\nOntology(<http://example.org/lc>\n  ClassAssertion(ObjectOneOf(:b :c) :a)\n  ClassAssertion(:P :b)\n  ClassAssertion(:P :c)\n  SubClassOf(:A ObjectOneOf(:a))\n  Declaration(NamedIndividual(:d1))\n  Declaration(NamedIndividual(:d2))\n  Declaration(NamedIndividual(:d3))\n  Declaration(NamedIndividual(:d4))\n)";
        let doc = crate::ofn_read::parse(ofn).unwrap();
        let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let resets = Arc::new(parking_lot::Mutex::new(
            Vec::<(bool, usize, usize, usize)>::new(),
        ));
        let r_cb = resets.clone();
        let mut sat = Saturation::new();
        let out = with_merge_probe(
            move |phase| {
                if let MergePhase::SplitPostLeafReset {
                    canon_want_none,
                    leaf_inds_cap,
                    interner_bytes,
                    residual_bytes,
                } = phase
                {
                    r_cb.lock().push((
                        canon_want_none,
                        leaf_inds_cap,
                        interner_bytes,
                        residual_bytes,
                    ));
                }
            },
            || {
                super::with_split_knobs(Some(0), Some(true), Some(4), Some(8), || {
                    sat.classify(
                        &refs,
                        None,
                        &Limits {
                            threads: 1,
                            rooted: true,
                            rooted_merge: true,
                            rooted_eq_split: true,
                            ..Limits::default()
                        },
                    )
                    .unwrap()
                })
            },
        );
        assert!(out.complete && out.stats.dl_rooted_splits >= 1 && out.stats.dl_rooted_leaves >= 2);
        let observed = resets.lock();
        assert!(
            observed.len() >= 2,
            "expected post-leaf reset events for both forked leaves, got {observed:?}"
        );
        for &(canon_none, inds_cap, interner_b, residual_b) in observed.iter() {
            assert!(
                canon_none,
                "canon_want must be cleared immediately on leaf reset"
            );
            assert_eq!(
                inds_cap, 0,
                "leaf_individuals capacity must be freed on leaf reset"
            );
            assert_eq!(
                interner_b, 0,
                "leaf reset must defer replacement interner init (interner_bytes == 0)"
            );
            assert_eq!(
                residual_b, 0,
                "leaf reset must leave zero residual outer bytes"
            );
        }
        assert!(
            sat.canon_want.is_none(),
            "canon_want must be None after SplitAnswer publication"
        );
        assert_eq!(
            sat.leaf_individuals.capacity(),
            0,
            "leaf_individuals capacity must be 0 after SplitAnswer publication"
        );
        assert_eq!(
            sat.interner.bytes(),
            0,
            "interner.bytes() must be 0 after SplitAnswer publication"
        );
        assert_eq!(
            sat.residual_outer_bytes(),
            0,
            "residual_outer_bytes() must be 0 after SplitAnswer publication"
        );
    }

    // 5. Sol r2 P1 #3 + Sol r3 P2 #2b: Site-anchored `RootStack` pre-admission refusal AND
    //    separate `SynthAxioms` (`DifferentIndividuals` `BTreeSet`) admission refusal.
    {
        let ofn = "Prefix(:=<http://example.org/st#>)\nOntology(<http://example.org/st>\n  ClassAssertion(ObjectOneOf(:b :c) :a)\n  ClassAssertion(:P :b)\n  ClassAssertion(:P :c)\n  SubClassOf(:A ObjectOneOf(:a))\n)";
        let doc = crate::ofn_read::parse(ofn).unwrap();
        let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let lim_base = Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            ..Limits::default()
        };

        // 5a. Root DFS stack refusal: arm pool immediately after `OrigSig` succeeds and
        //     assert `SplitAlloc { site: RootStack, admitted: false }` fires before saturation.
        let armed_root = Arc::new(AtomicBool::new(false));
        let root_refused_site = Arc::new(parking_lot::Mutex::new(None::<(usize, bool)>));
        let ar_cb = armed_root.clone();
        let ar_probe = armed_root.clone();
        let rrs = root_refused_site.clone();
        let mut sat_ref = Saturation::new();
        let err = with_merge_probe(
            move |p| match p {
                MergePhase::SplitAlloc {
                    site: SplitAllocSite::OrigSig,
                    admitted: true,
                    ..
                } => {
                    ar_probe.store(true, Ordering::Release);
                }
                MergePhase::SplitAlloc {
                    site: SplitAllocSite::RootStack,
                    proposed,
                    admitted,
                } => {
                    *rrs.lock() = Some((proposed, admitted));
                }
                _ => {}
            },
            || {
                sat_ref
                    .classify(
                        &refs,
                        None,
                        &Limits {
                            charge: Some(Arc::new(move |_more: usize| {
                                !ar_cb.load(Ordering::Acquire)
                            })),
                            ..lim_base.clone()
                        },
                    )
                    .unwrap_err()
            },
        );
        assert!(
            matches!(err, Error::Memory { .. }),
            "refusing root DFS stack allocation must return Error::Memory, got {err:?}"
        );
        let root_site = *root_refused_site.lock();
        assert!(
            matches!(root_site, Some((proposed, false)) if proposed > 0),
            "refusal must anchor specifically to SplitAlloc {{ site: RootStack, admitted: false }}, got {root_site:?}"
        );
        assert!(
            !sat_ref.dl_tried,
            "root DFS stack refusal must occur before branch saturation starts"
        );

        // 5b. Separate synthetic `DifferentIndividuals` `BTreeSet` admission refusal:
        //     Keep `:a` long (256 KiB) and `:b1..:b5` short so `:a` appears once in
        //     `interner.individuals` (~256 KiB), whereas each `B_ne` depth adds another
        //     `(:a, :bk)` pair (+256 KiB in `frame.neq` and +512 KiB in `SynthAxioms`),
        //     so `SynthAxioms` strictly exceeds every prior watermark and `max_memory`
        //     refuses specifically at `SplitAlloc { site: SynthAxioms, admitted: false }`.
        let pad256 = "z".repeat(256 * 1024);
        let ofn_sy = format!(
            "Prefix(:=<http://example.org/sy3#>)\n\
             Ontology(<http://example.org/sy3>\n\
               ClassAssertion(ObjectOneOf(:b1 :b2 :b3 :b4 :b5) :a_{pad256})\n\
               ClassAssertion(:P :b1)\n\
               ClassAssertion(:P :b2)\n\
               ClassAssertion(:P :b3)\n\
               ClassAssertion(:P :b4)\n\
               ClassAssertion(:P :b5)\n\
               SubClassOf(:A ObjectOneOf(:a_{pad256}))\n\
             )"
        );
        let doc_sy = crate::ofn_read::parse(&ofn_sy).unwrap();
        let refs_sy: Vec<&AxiomKind> = doc_sy.axioms.iter().map(|a| &a.kind).collect();
        let sy_events = Arc::new(parking_lot::Mutex::new(Vec::<MergePhase>::new()));
        let se_cb = sy_events.clone();
        let mut sat_sy = Saturation::new();
        let sy_ok = with_merge_probe(
            move |p| se_cb.lock().push(p),
            || {
                super::with_split_knobs(Some(0), Some(true), Some(6), Some(16), || {
                    sat_sy
                        .classify(
                            &refs_sy,
                            None,
                            &Limits {
                                charge: Some(Arc::new(|_more: usize| true)),
                                ..lim_base.clone()
                            },
                        )
                        .unwrap()
                })
            },
        );
        assert!(sy_ok.complete && sy_ok.stats.dl_rooted_splits >= 3);
        let evs = sy_events.lock().clone();
        let mut best_sy_before = 0usize;
        let mut best_sy_prop = 0usize;
        let mut last_paid_before = 0usize;
        for p in &evs {
            match *p {
                MergePhase::LedgerCharge { paid_before, .. } => {
                    last_paid_before = paid_before;
                }
                MergePhase::SplitAlloc {
                    site: SplitAllocSite::SynthAxioms,
                    proposed,
                    admitted: true,
                } if proposed > best_sy_prop => {
                    best_sy_prop = proposed;
                    best_sy_before = last_paid_before;
                }
                _ => {}
            }
        }
        assert!(
            best_sy_prop > best_sy_before + 1024,
            "SynthAxioms proposed ({best_sy_prop}) must strictly exceed prior watermark ({best_sy_before})"
        );
        let sy_cap = best_sy_before + (best_sy_prop - best_sy_before) / 2;
        let saw_sy_refused = Arc::new(parking_lot::Mutex::new(None::<(usize, bool)>));
        let ssr = saw_sy_refused.clone();
        let mut sat_sy_cap = Saturation::new();
        let sy_res = with_merge_probe(
            move |p| {
                if let MergePhase::SplitAlloc {
                    site: SplitAllocSite::SynthAxioms,
                    proposed,
                    admitted: false,
                } = p
                {
                    *ssr.lock() = Some((proposed, false));
                }
            },
            || {
                super::with_split_knobs(Some(0), Some(true), Some(6), Some(16), || {
                    sat_sy_cap.classify(
                        &refs_sy,
                        None,
                        &Limits {
                            max_memory: Some(sy_cap),
                            ..lim_base.clone()
                        },
                    )
                })
            },
        );
        let ssr_val = *saw_sy_refused.lock();
        assert!(
            matches!(ssr_val, Some((proposed, false)) if proposed > sy_cap),
            "synthetic DifferentIndividuals BTreeSet refusal must fire at SplitAlloc {{ site: SynthAxioms, admitted: false }}, got {ssr_val:?}"
        );
        match sy_res {
            Ok(fb) => assert!(!fb.complete && fb.stats.dl_over_budget),
            Err(Error::Memory { used, .. }) => assert!(used > sy_cap),
            Err(other) => panic!("unexpected error on SynthAxioms refusal: {other:?}"),
        }
    }

    // 6. Sol r3 P1 #1: Long-IRI `STOP_SPLIT` handoff refusal fixture with counting-allocator
    //    assertion proving ZERO unbudgeted long-IRI copies occur in `run_once` or the
    //    `STOP_SPLIT` handoff before admission at `ForkNeq`.
    //    `SubClassOf(:X ObjectUnionOf(:B :C))` (zero individuals in `scope_bytes`) combined
    //    with pure-EL singleton nominals `SubClassOf(:B ObjectOneOf(:b))` and
    //    `SubClassOf(:C ObjectOneOf(:c))` plus `ClassAssertion(:X :a)` over 512 KiB IRIs
    //    derives `:a = :b ∨ :a = :c` with ZERO long IRIs in `scope_bytes`. Root saturation
    //    peaks at `orig_sig (6L) + interner (3L) + ~0.1 MiB ≈ 4.6 MiB` while `ForkNeq`
    //    proposes `orig_sig (6L) + stop_bytes (2L) + ne_add (2L) ≈ 5.0 MiB`.
    //    Under `max_memory = neq_prop - 128 KiB`, root saturation reaches `STOP_SPLIT`,
    //    `StopHandoff` allocates <4 KiB (zero 1 MiB IRI clone), and `ForkNeq` refuses BEFORE
    //    cloning the two 512 KiB IRIs!
    {
        let pad512 = "k".repeat(512 * 1024);
        let ofn = format!(
            "Prefix(:=<http://example.org/handoff#>)\n\
             Ontology(<http://example.org/handoff>\n\
               SubClassOf(:X ObjectUnionOf(:B :C))\n\
               SubClassOf(:B ObjectOneOf(:b_{pad512}))\n\
               SubClassOf(:C ObjectOneOf(:c_{pad512}))\n\
               ClassAssertion(:X :a_{pad512})\n\
               ClassAssertion(:P :b_{pad512})\n\
               ClassAssertion(:P :c_{pad512})\n\
               SubClassOf(:A ObjectOneOf(:a_{pad512}))\n\
             )"
        );
        let doc = crate::ofn_read::parse(&ofn).unwrap();
        let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let lim_base = Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            ..Limits::default()
        };

        // Measure the exact pre-`ForkNeq` ledger max vs `ForkNeq` proposed live total.
        let fork_neq_prop = Arc::new(AtomicUsize::new(0));
        let pre_neq_max = Arc::new(AtomicUsize::new(0));
        let past_clone = Arc::new(AtomicBool::new(false));
        let fnp = fork_neq_prop.clone();
        let pnm = pre_neq_max.clone();
        let pc = past_clone.clone();
        let mut sat_meas = Saturation::new();
        let meas_out = with_merge_probe(
            move |phase| match phase {
                MergePhase::SplitAlloc {
                    site: SplitAllocSite::ForkMergeClone,
                    admitted: true,
                    ..
                } => {
                    pc.store(true, Ordering::Release);
                }
                MergePhase::SplitAlloc {
                    site: SplitAllocSite::ForkNeq,
                    proposed,
                    admitted: true,
                } if fnp.load(Ordering::Acquire) == 0 => {
                    fnp.store(proposed, Ordering::Release);
                }
                MergePhase::LedgerCharge { used, .. } if !pc.load(Ordering::Acquire) => {
                    pnm.fetch_max(used, Ordering::AcqRel);
                }
                _ => {}
            },
            || {
                super::with_split_knobs(Some(0), Some(true), Some(2), Some(4), || {
                    sat_meas.classify(&refs, None, &lim_base).unwrap()
                })
            },
        );
        assert!(meas_out.complete && meas_out.stats.dl_rooted_splits >= 1);
        let neq_prop = fork_neq_prop.load(Ordering::Acquire);
        // Root saturation's live peak is `orig_sig (3 MiB) + interner (1.5 MiB) + ~0.1 MiB`,
        // well below `neq_prop` (~5 MiB); pick a hard cap 128 KiB below `neq_prop`.
        assert!(
            neq_prop > 4 * 1024 * 1024,
            "expected ForkNeq proposed > 4 MiB with 512 KiB IRIs, got {neq_prop}"
        );
        let handoff_cap = neq_prop - 128 * 1024;
        assert!(
            handoff_cap > pre_neq_max.load(Ordering::Acquire),
            "handoff_cap ({handoff_cap}) must exceed pre-ForkNeq ledger charges ({})",
            pre_neq_max.load(Ordering::Acquire)
        );

        let alloc_at_sat_stop = Arc::new(AtomicUsize::new(0));
        let alloc_at_handoff = Arc::new(AtomicUsize::new(0));
        let alloc_at_neq_refusal = Arc::new(AtomicUsize::new(0));
        let neq_refused = Arc::new(parking_lot::Mutex::new(None::<(usize, bool)>));

        let a_sat = alloc_at_sat_stop.clone();
        let a_ho = alloc_at_handoff.clone();
        let a_neq = alloc_at_neq_refusal.clone();
        let nr_p = neq_refused.clone();

        let mut sat = Saturation::new();
        let (res, _sample) = measure(|| {
            with_merge_probe(
                move |phase| match phase {
                    MergePhase::SatStop { stop, .. } if stop == super::saturate::STOP_SPLIT => {
                        a_sat.store(sample().requested_alloc_bytes, Ordering::Release);
                    }
                    MergePhase::SplitAlloc {
                        site: SplitAllocSite::StopHandoff,
                        admitted: true,
                        ..
                    } => {
                        a_ho.store(sample().requested_alloc_bytes, Ordering::Release);
                    }
                    MergePhase::SplitAlloc {
                        site: SplitAllocSite::ForkNeq,
                        proposed,
                        admitted: false,
                    } => {
                        a_neq.store(sample().requested_alloc_bytes, Ordering::Release);
                        *nr_p.lock() = Some((proposed, false));
                    }
                    _ => {}
                },
                || {
                    super::with_split_knobs(Some(0), Some(true), Some(2), Some(4), || {
                        sat.classify(
                            &refs,
                            None,
                            &Limits {
                                max_memory: Some(handoff_cap),
                                ..lim_base
                            },
                        )
                    })
                },
            )
        });

        let sat_bytes = alloc_at_sat_stop.load(Ordering::Acquire);
        let ho_bytes = alloc_at_handoff.load(Ordering::Acquire);
        let neq_bytes = alloc_at_neq_refusal.load(Ordering::Acquire);
        assert!(
            sat_bytes > 0 && ho_bytes >= sat_bytes,
            "must observe SatStop(STOP_SPLIT) followed by StopHandoff: sat={sat_bytes}, ho={ho_bytes}"
        );
        // Before our fix, `run_once` cloned two 512 KiB IRIs (>1 MiB) right after `SatStop`!
        assert!(
            ho_bytes - sat_bytes < 4096,
            "STOP_SPLIT handoff must not clone the two 512 KiB IRIs before admission (allocated {} bytes between SatStop and StopHandoff)",
            ho_bytes - sat_bytes
        );
        let nr_val = *neq_refused.lock();
        assert!(
            matches!(nr_val, Some((proposed, false)) if proposed > handoff_cap),
            "refusal at ForkNeq must fire before cloning the two 512 KiB IRIs, got {nr_val:?}"
        );
        assert!(
            neq_bytes - sat_bytes < 8192,
            "refused ForkNeq must allocate neither the handoff copies nor the ne_neq copies (allocated {} bytes)",
            neq_bytes - sat_bytes
        );
        match res {
            Ok(fb) => {
                assert!(!fb.complete && fb.stats.dl_over_budget);
                assert!(sat.split_answer.is_none());
            }
            Err(Error::Memory { used, .. }) => {
                assert!(used > handoff_cap);
                assert!(sat.split_answer.is_none());
            }
            Err(other) => panic!("unexpected error on long-IRI STOP_SPLIT refusal: {other:?}"),
        }
    }
}

#[test]
fn split_round4_reused_interner_owns_role_iri() {
    use super::admission_test::{MergePhase, SplitAllocSite, with_merge_probe};

    let pad256k = "r".repeat(256 * 1024);
    let prior_ofn = format!(
        "Prefix(:=<http://example.org/rp#>)\n\
         Ontology(<http://example.org/rp>\n\
           ObjectPropertyAssertion(:p_{pad256k} :x :y)\n\
         )"
    );
    let prior_doc = crate::ofn_read::parse(&prior_ofn).unwrap();
    let prior_refs: Vec<&AxiomKind> = prior_doc.axioms.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let prior_out = sat
        .classify(
            &prior_refs,
            None,
            &Limits {
                threads: 1,
                el_only: true,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(prior_out.complete);
    let role_iri_bytes = pad256k.len();
    let entry_live = sat.leaf_live_bytes_without_canon_and_merge();
    assert!(
        entry_live >= role_iri_bytes,
        "entry estimate ({entry_live}) must include the owned role IRI ({role_iri_bytes})"
    );

    let mut small_ax = Vec::new();
    let a = crate::model::Individual::named("http://example.org/rps#a");
    let b = crate::model::Individual::named("http://example.org/rps#b");
    small_ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
        crate::model::ClassExpression::ObjectOneOf(std::collections::BTreeSet::from([
            a.clone(),
            b,
        ])),
        a,
    )));
    for i in 0..40usize {
        small_ax.push(crate::model::Axiom::new(AxiomKind::Declaration(
            crate::model::Entity::Class(Class::new(format!(
                "http://example.org/rps#S{i:02}_{}",
                "y".repeat(64)
            ))),
        )));
    }
    let small_refs: Vec<&AxiomKind> = small_ax.iter().map(|x| &x.kind).collect();
    let cap_reuse = entry_live.saturating_add(3072);
    let lim_reuse = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        max_memory: Some(cap_reuse),
        ..Limits::default()
    };
    let mut sat_fresh = Saturation::new();
    assert!(
        sat_fresh
            .classify(&small_refs, None, &lim_reuse)
            .unwrap()
            .complete,
        "fresh saturation must fit easily under cap_reuse={cap_reuse}"
    );
    let orig_sig_ev = Arc::new(parking_lot::Mutex::new(None::<(usize, bool)>));
    let ose = orig_sig_ev.clone();
    let reuse_err = with_merge_probe(
        move |p| {
            if let MergePhase::SplitAlloc {
                site: SplitAllocSite::OrigSig,
                proposed,
                admitted,
            } = p
            {
                *ose.lock() = Some((proposed, admitted));
            }
        },
        || sat.classify(&small_refs, None, &lim_reuse).unwrap_err(),
    );
    assert!(
        matches!(reuse_err, Error::Memory { used, .. } if used > cap_reuse),
        "reused saturation must refuse when prior role IRI bytes ({role_iri_bytes}) exceed cap {cap_reuse}, got {reuse_err:?}"
    );
    assert!(
        matches!(*orig_sig_ev.lock(), Some((proposed, false)) if proposed > cap_reuse),
        "role-IRI reuse refusal must occur at OrigSig with proposed > {cap_reuse}, got {:?}",
        *orig_sig_ev.lock()
    );
    assert_eq!(
        sat.interner.bytes(),
        0,
        "terminal refused classify must leave interner empty"
    );
}

#[test]
fn split_round4_leaf_export_charges_role_iri() {
    use super::admission_test::{MergePhase, SplitAllocSite, with_merge_probe};

    let pad320k = "q".repeat(320 * 1024);
    let mut lf_ax = Vec::new();
    let a = crate::model::Individual::named("http://example.org/rlf#a");
    let b = crate::model::Individual::named("http://example.org/rlf#b");
    let c = crate::model::Individual::named("http://example.org/rlf#c");
    let rep = crate::model::Individual::named("http://example.org/rlf#rep");
    let root = Class::new("http://example.org/rlf#Root");
    let long_prop =
        crate::model::ObjectProperty::new(format!("http://example.org/rlf#rel_{pad320k}"));
    lf_ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
        crate::model::ClassExpression::ObjectOneOf(std::collections::BTreeSet::from([
            b.clone(),
            c.clone(),
        ])),
        a.clone(),
    )));
    lf_ax.push(crate::model::Axiom::new(
        AxiomKind::ObjectPropertyAssertion(
            crate::model::ObjectPropertyExpression::Property(long_prop),
            a.clone(),
            b.clone(),
        ),
    ));
    lf_ax.push(crate::model::Axiom::new(AxiomKind::ClassAssertion(
        crate::model::ClassExpression::Class(root.clone()),
        rep.clone(),
    )));
    for i in 0..300usize {
        let ci = Class::new(format!("http://example.org/rlf#C{i:03}"));
        lf_ax.push(crate::model::Axiom::sub_class_of(root.clone(), ci));
    }
    for i in 0..400usize {
        let alias = crate::model::Individual::named(format!("http://example.org/rlf#i{i:03}"));
        lf_ax.push(crate::model::Axiom::new(AxiomKind::SameIndividual(
            std::collections::BTreeSet::from([rep.clone(), alias]),
        )));
    }
    let lf_refs: Vec<&AxiomKind> = lf_ax.iter().map(|x| &x.kind).collect();
    let lim_lf = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        ..Limits::default()
    };
    let export_watermarks = |refs: &[&AxiomKind]| {
        let tally = Arc::new(AtomicUsize::new(0));
        let charge = tally.clone();
        let watermarks = Arc::new(parking_lot::Mutex::new((0, 0, 0)));
        let record = watermarks.clone();
        let out = with_merge_probe(
            move |phase| {
                let mut record = record.lock();
                match phase {
                    MergePhase::SplitAlloc {
                        site: SplitAllocSite::LeafLive,
                        proposed,
                        admitted: true,
                    } if record.0 == 0 => record.0 = proposed,
                    MergePhase::SplitLeafExportStart { leaves: 0 } => {
                        record.1 = tally.load(Ordering::Relaxed);
                    }
                    MergePhase::SplitLeafExportDone { leaves: 0 } => {
                        record.2 = tally.load(Ordering::Relaxed);
                    }
                    _ => {}
                }
            },
            || {
                super::with_split_knobs(Some(0), Some(true), Some(2), Some(4), || {
                    Saturation::new()
                        .classify(
                            refs,
                            None,
                            &Limits {
                                charge: Some(Arc::new(move |more| {
                                    charge.fetch_add(more, Ordering::Relaxed);
                                    true
                                })),
                                ..lim_lf.clone()
                            },
                        )
                        .unwrap()
                })
            },
        );
        assert!(out.complete && out.stats.dl_rooted_splits >= 1);
        *watermarks.lock()
    };

    let short_property = AxiomKind::ObjectPropertyAssertion(
        crate::model::ObjectPropertyExpression::Property(crate::model::ObjectProperty::new(
            "http://example.org/rlf#rel_",
        )),
        a,
        b,
    );
    let mut short_refs = lf_refs.clone();
    short_refs[1] = &short_property;
    let (short_live, _, _) = export_watermarks(&short_refs);
    let (long_live, pre_w, post_w) = export_watermarks(&lf_refs);
    assert!(short_live > 0);
    assert!(
        long_live >= short_live + pad320k.len(),
        "LeafLive grew from {short_live} to {long_live} for an extra {} role IRI bytes",
        pad320k.len()
    );
    assert!(
        post_w > pre_w + 512,
        "role-IRI leaf export watermark ({post_w}) must exceed pre-export ({pre_w})"
    );
    let lf_cap = pre_w + (post_w - pre_w) / 2;
    let saw_ref = Arc::new(AtomicBool::new(false));
    let sr = saw_ref.clone();
    let mut sat_cap = Saturation::new();
    let res = with_merge_probe(
        move |p| {
            if matches!(p, MergePhase::SplitLeafExportRefused) {
                sr.store(true, Ordering::Release);
            }
        },
        || {
            super::with_split_knobs(Some(0), Some(true), Some(2), Some(4), || {
                sat_cap.classify(
                    &lf_refs,
                    None,
                    &Limits {
                        max_memory: Some(lf_cap),
                        ..lim_lf
                    },
                )
            })
        },
    );
    assert!(
        saw_ref.load(Ordering::Acquire),
        "long-role-IRI leaf export must refuse under overlap cap {lf_cap}"
    );
    match res {
        Ok(fb) => assert!(!fb.complete && fb.stats.dl_over_budget),
        Err(Error::Memory { used, .. }) => assert!(used > lf_cap),
        Err(other) => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn split_round4_split_handoff_keeps_exclusions_live() {
    use super::admission_test::{MergePhase, SplitAllocSite, with_merge_probe};

    let pad512k = "e".repeat(512 * 1024);
    let ofn = format!(
        "Prefix(:=<http://example.org/exsplit#>)\n\
         Ontology(<http://example.org/exsplit>\n\
           ClassAssertion(ObjectOneOf(:b :c) :a)\n\
           IrreflexiveObjectProperty(:irref_{pad512k})\n\
           ClassAssertion(:P :b)\n\
           ClassAssertion(:P :c)\n\
           SubClassOf(:A ObjectOneOf(:a))\n\
         )"
    );
    let doc = crate::ofn_read::parse(&ofn).unwrap();
    let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
    let lim = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        ..Limits::default()
    };
    let stop_prop = Arc::new(AtomicUsize::new(0));
    let fork_neq_prop = Arc::new(AtomicUsize::new(0));
    let sp = stop_prop.clone();
    let fnp = fork_neq_prop.clone();
    let mut sat_meas = Saturation::new();
    let out_meas = with_merge_probe(
        move |p| match p {
            MergePhase::SplitAlloc {
                site: SplitAllocSite::StopHandoff,
                proposed,
                admitted: true,
            } => {
                sp.fetch_max(proposed, Ordering::AcqRel);
            }
            MergePhase::SplitAlloc {
                site: SplitAllocSite::ForkNeq,
                proposed,
                admitted: true,
            } => {
                fnp.fetch_max(proposed, Ordering::AcqRel);
            }
            _ => {}
        },
        || {
            super::with_split_knobs(Some(0), Some(true), Some(4), Some(8), || {
                sat_meas.classify(&refs, None, &lim).unwrap()
            })
        },
    );
    assert!(!out_meas.complete);
    assert_eq!(sat_meas.excluded().len(), 1);
    assert!(
        stop_prop.load(Ordering::Acquire) > 512 * 1024,
        "STOP_SPLIT StopHandoff proposed ({}) must include live 512 KiB self.excluded",
        stop_prop.load(Ordering::Acquire)
    );
    assert!(
        fork_neq_prop.load(Ordering::Acquire) > 512 * 1024,
        "STOP_SPLIT ForkNeq proposed ({}) must keep live 512 KiB self.excluded debited",
        fork_neq_prop.load(Ordering::Acquire)
    );
}

#[test]
fn split_round4_public_split_abort_leaves_empty_interner() {
    use super::admission_test::{MergePhase, with_merge_probe};

    let input = axioms(
        "ClassAssertion(ObjectOneOf(:b :c) :a)
         IrreflexiveObjectProperty(:irref)
         ClassAssertion(:P :b)
         ClassAssertion(:P :c)
         SubClassOf(:A ObjectOneOf(:a))",
    );
    let refs: Vec<_> = input.iter().map(|axiom| &axiom.kind).collect();
    let cancel_at_split = Arc::new(AtomicBool::new(false));
    let c_cb = cancel_at_split.clone();
    let c_pr = cancel_at_split.clone();
    let mut sat_cancel = Saturation::new();
    let cancel_err = with_merge_probe(
        move |p| {
            if matches!(p, MergePhase::SatStop { stop, .. } if stop == super::saturate::STOP_SPLIT)
            {
                c_pr.store(true, Ordering::Release);
            }
        },
        || {
            super::with_split_knobs(Some(0), Some(true), Some(4), Some(8), || {
                sat_cancel
                    .classify(
                        &refs,
                        None,
                        &Limits {
                            cancelled: Some(Arc::new(move || c_cb.load(Ordering::Acquire))),
                            threads: 1,
                            rooted: true,
                            rooted_merge: true,
                            rooted_eq_split: true,
                            ..Limits::default()
                        },
                    )
                    .unwrap_err()
            })
        },
    );
    assert!(
        matches!(cancel_err, Error::Cancelled),
        "expected Error::Cancelled on STOP_SPLIT handoff abort, got {cancel_err:?}"
    );
    assert_eq!(sat_cancel.excluded().len(), 1);
    assert_eq!(sat_cancel.excluded()[0].axiom, *refs[1]);
    assert_eq!(
        sat_cancel.interner.bytes(),
        0,
        "terminal error path must leave interner empty (no eager reset())"
    );
}

#[test]
fn split_round4_merge_handoff_keeps_exclusions_live() {
    use super::admission_test::{MergePhase, SplitAllocSite, with_merge_probe};

    let pad512k = "m".repeat(512 * 1024);
    let ofn = format!(
        "Prefix(:=<http://example.org/exmerge#>)\n\
         Ontology(<http://example.org/exmerge>\n\
           SubClassOf(:D ObjectUnionOf(:E :F))\n\
           SubClassOf(:X ObjectOneOf(:b))\n\
           ClassAssertion(:X :a)\n\
           IrreflexiveObjectProperty(:irref_{pad512k})\n\
           ClassAssertion(:P :b)\n\
           SubClassOf(:A ObjectOneOf(:a))\n\
           Declaration(NamedIndividual(:c))\n\
         )"
    );
    let doc = crate::ofn_read::parse(&ofn).unwrap();
    let refs: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
    let lim = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        ..Limits::default()
    };
    let mh_prop = Arc::new(AtomicUsize::new(0));
    let mc_prop = Arc::new(AtomicUsize::new(0));
    let mh = mh_prop.clone();
    let mc = mc_prop.clone();
    let mut sat = Saturation::new();
    let out = with_merge_probe(
        move |p| match p {
            MergePhase::SplitAlloc {
                site: SplitAllocSite::MergeHandoff,
                proposed,
                admitted: true,
            } => {
                mh.fetch_max(proposed, Ordering::AcqRel);
            }
            MergePhase::SplitAlloc {
                site: SplitAllocSite::MergeCanon,
                proposed,
                admitted: true,
            } => {
                mc.fetch_max(proposed, Ordering::AcqRel);
            }
            _ => {}
        },
        || sat.classify(&refs, None, &lim).unwrap(),
    );
    assert!(!out.complete && out.stats.dl_rooted_restarts >= 1);
    assert_eq!(sat.excluded().len(), 1);
    assert!(
        mh_prop.load(Ordering::Acquire) > 512 * 1024,
        "STOP_MERGE MergeHandoff proposed ({}) must include live 512 KiB self.excluded",
        mh_prop.load(Ordering::Acquire)
    );
    assert!(
        mc_prop.load(Ordering::Acquire) > 512 * 1024,
        "STOP_MERGE MergeCanon proposed ({}) must keep live 512 KiB self.excluded in canon_base",
        mc_prop.load(Ordering::Acquire)
    );
}

#[test]
fn split_round4_declined_forked_plan_leaves_empty_interner() {
    use super::admission_test::{MergePhase, PlanKind, with_merge_probe};

    let input = axioms(
        "HasKey(:K (:r) ())
         ClassAssertion(ObjectOneOf(:b :c) :a)",
    );
    let refs: Vec<_> = input.iter().map(|axiom| &axiom.kind).collect();
    let mut sat = Saturation::new();
    assert!(sat.interner.bytes() > 0);
    // Supply the driver's forked state to inspect the inner error before the driver
    // performs its own cleanup. The public split-stop cancellation covers that cleanup.
    sat.split_cfg = Some(SplitBranchCfg {
        mode: dl_rules::SplitMode::EagerPureThenProbe,
        probe: 0,
        epoch: 1,
        forked: true,
        orig_individuals: Arc::default(),
        neq: Vec::new(),
    });
    let limits = Limits {
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        threads: 1,
        max_memory: Some(0),
        ..Limits::default()
    };
    let outcome = Arc::new(parking_lot::Mutex::new(None));
    let record = outcome.clone();
    let result = with_merge_probe(
        move |phase| {
            if let MergePhase::PlanDone { outcome, .. } = phase {
                *record.lock() = Some(outcome);
            }
        },
        || {
            sat.run(
                &refs,
                None,
                0,
                &limits,
                &Arc::new(AtomicUsize::new(0)),
                &mut 0,
                false,
                AttemptProvenance::Ineligible,
            )
        },
    );
    assert_eq!(*outcome.lock(), Some(PlanKind::Declined));
    assert!(matches!(result, Err(Error::Memory { .. })));
    assert!(sat.split_ineligible);
    assert_eq!(sat.interner.bytes(), 0);
}

#[test]
fn split_round6_reduced_merge_does_not_copy_unchanged_exclusions() {
    use super::admission_test::{MergePhase, PlanKind, SplitAllocSite, with_merge_probe};

    const IRI_BYTES: usize = 512 * 1024;
    let input = axioms(&format!(
        "SubClassOf(:D ObjectUnionOf(:E :F))
         SubClassOf(:X ObjectOneOf(:b))
         ClassAssertion(:X :a)
         HasKey(:K (:r) ())
         IrreflexiveObjectProperty(:irref_{})",
        "m".repeat(IRI_BYTES)
    ));
    let refs: Vec<_> = input.iter().map(|a| &a.kind).collect();
    let excluded = refs
        .iter()
        .find(|k| matches!(k, AxiomKind::IrreflexiveObjectProperty(_)))
        .unwrap();
    // The interner keeps an axiom copy for its normalization scope. Leave room for
    // that copy and one report while the first branch is live.
    let cap = 2 * IRI_BYTES + IRI_BYTES / 4;
    let limits = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        max_memory: Some(cap),
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let ready = Arc::new(AtomicUsize::new(0));
    let planned = ready.clone();
    let handoff_allocated = Arc::new(parking_lot::Mutex::new(None));
    let observed = handoff_allocated.clone();
    let mut at_stop = None;
    let result = with_merge_probe(
        move |phase| match phase {
            MergePhase::PlanDone {
                outcome: PlanKind::Ready,
                ..
            } => {
                planned.fetch_add(1, Ordering::Relaxed);
            }
            MergePhase::SatStop { stop, .. } if stop == super::saturate::STOP_MERGE => {
                at_stop = Some(sample().requested_alloc_bytes);
            }
            MergePhase::SplitAlloc {
                site: SplitAllocSite::MergeHandoff,
                admitted: true,
                ..
            } => {
                *observed.lock() = at_stop.map(|bytes| sample().requested_alloc_bytes - bytes);
            }
            _ => {}
        },
        || {
            measure(|| {
                sat.run_splitting(
                    &refs,
                    None,
                    0,
                    &limits,
                    &Arc::new(AtomicUsize::new(0)),
                    &mut 0,
                    true,
                    AttemptProvenance::Ineligible,
                )
            })
            .0
        },
    );
    assert_eq!(
        ready.load(Ordering::Relaxed),
        2,
        "both reduction plans must be ready"
    );
    assert!(
        matches!(result, Err(Error::Memory { limit, used }) if limit == cap && used > cap),
        "the restarted build must refuse a second report, got {result:?}"
    );
    assert_eq!(sat.excluded().len(), 1);
    assert!(sat.excluded()[0].axiom == **excluded);

    let allocated = handoff_allocated
        .lock()
        .expect("saturation must hand off a merge");
    assert!(
        allocated < IRI_BYTES,
        "restoring the unchanged report allocated {allocated} bytes before the merge handoff"
    );
}

#[test]
fn reduced_exclusion_restore_accounts_for_taken_profile_diagnostics() {
    let input = axioms(
        "SubClassOf(:D ObjectUnionOf(:E :F))
         DisjointClasses(
             ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
             ObjectMinCardinality(2 :r ObjectMinCardinality(1 owl:topObjectProperty :A)))
         SubClassOf(
             ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
             owl:Nothing)
         ClassAssertion(:B :b)",
    );
    let original: Vec<_> = input.iter().map(|a| &a.kind).collect();
    let mut red = dl_reduce::plan(&original, None, None).expect("ready reduction");
    let hub = red.hub.take();
    let reduced = red.all(&original);
    let mut outside: Vec<_> = reduced
        .iter()
        .enumerate()
        .filter_map(|(i, k)| el_break(k).map(|b| (i, (b.rule.to_string(), b.message))))
        .collect();
    let union = outside
        .iter()
        .position(|(i, _)| reduced[*i] == original[0])
        .expect("profile diagnostic for the disjunction");
    let previous = axioms("IrreflexiveObjectProperty(:old)");
    let old_report = vec![Excluded {
        axiom: previous[0].kind.clone(),
        gap: Gap::Outside {
            rule: "previous rule".into(),
            message: "previous diagnostic".into(),
        },
    }];
    let held_bytes = excluded_copy_bytes(&old_report)
        + red.retained_bytes()
        + reduced.capacity() * std::mem::size_of::<&AxiomKind>();
    let build = |outside: &Vec<_>, cap| {
        let mut sat = Saturation::new();
        sat.active_hub = hub.clone();
        sat.hide_internal = true;
        sat.excluded = old_report.clone();
        let result = sat.build(
            &reduced,
            outside,
            Some((&red, &original)),
            1,
            held_bytes,
            0,
            &Limits {
                max_memory: cap,
                rooted: true,
                threads: 1,
                ..Limits::default()
            },
            AttemptProvenance::Ineligible,
        );
        (sat, result)
    };

    // Put the report overlap above the normalization peak without changing the
    // report itself. Tier D takes this disjunction so its diagnostic is filtered out.
    let message = &mut outside[union].1.1;
    message.reserve_exact(64 * 1024);
    let control_capacity = message.capacity();
    let (sat, result) = build(&outside, None);
    let (_, _, peak, _, _) = result.expect("initial build");
    assert_eq!(sat.excluded().len(), 2);
    for axiom in &original[1..3] {
        assert!(sat.excluded().iter().any(|e| &e.axiom == *axiom));
    }

    let message = &mut outside[union].1.1;
    message.reserve_exact(2 * 64 * 1024);
    let restore_bytes = peak + message.capacity() - control_capacity;
    let cap = restore_bytes - 1;
    let (sat, result) = build(&outside, Some(cap));
    match result {
        Err(Error::Memory { limit, used }) => assert_eq!((limit, used), (cap, restore_bytes)),
        Err(err) => panic!("expected exclusion restoration to refuse, got {err:?}"),
        Ok(_) => panic!("restored the report without admitting the retained diagnostic"),
    }
    assert_eq!(sat.excluded(), old_report);
    assert!(!sat.report_replaced);
}

#[test]
fn split_round5_replacement_exclusions_are_admitted_before_cloning() {
    const IRI_BYTES: usize = 512 * 1024;
    let old = axioms(&format!(
        "IrreflexiveObjectProperty(:old_{})",
        "o".repeat(IRI_BYTES)
    ));
    let new = axioms(&format!(
        "IrreflexiveObjectProperty(:new_{})",
        "n".repeat(IRI_BYTES)
    ));
    let old_refs = [&old[0].kind];
    let new_refs = [&new[0].kind];
    let limits = Limits {
        el_only: true,
        threads: 1,
        ..Limits::default()
    };
    let mut sat = Saturation::new();
    let old_peak = sat
        .saturate(&old_refs, None, &limits)
        .unwrap()
        .stats
        .peak_memory;
    let new_peak = Saturation::new()
        .saturate(&new_refs, None, &limits)
        .unwrap()
        .stats
        .peak_memory;
    let cap = old_peak.max(new_peak) + IRI_BYTES / 8;
    assert!(cap < 2 * IRI_BYTES, "the cap must exclude two reports");
    let bounded = Limits {
        max_memory: Some(cap),
        ..limits
    };
    Saturation::new()
        .saturate(&old_refs, None, &bounded)
        .unwrap();
    let mut fresh = Saturation::new();
    let (one_report, admitted) = measure(|| fresh.saturate(&new_refs, None, &bounded));
    one_report.unwrap();

    // A restarted build drops the old index and contexts but keeps the public report.
    sat.reset_empty();
    let (result, sample) = measure(|| sat.saturate(&new_refs, None, &bounded));
    assert!(
        matches!(result, Err(Error::Memory { limit, used }) if limit == cap && used > cap),
        "replacement should exceed the cap, got {result:?}"
    );
    assert!(
        sample.requested_alloc_bytes + IRI_BYTES <= admitted.requested_alloc_bytes,
        "refused build allocated {} bytes versus {} with one report",
        sample.requested_alloc_bytes,
        admitted.requested_alloc_bytes
    );
    assert_eq!(sat.excluded().len(), 1);
    assert!(sat.excluded()[0].axiom == *old_refs[0]);
}

#[test]
fn split_round5_retained_exclusions_reduce_taxonomy_headroom() {
    let input = axioms(&format!(
        "SubClassOf(:D ObjectUnionOf(:E :F))
         SubClassOf(:X ObjectOneOf(:b))
         ClassAssertion(:X :a)
         IrreflexiveObjectProperty(:irref_{})
         ClassAssertion(:P :b)
         SubClassOf(:A ObjectOneOf(:a))",
        "m".repeat(512 * 1024)
    ));
    let refs: Vec<_> = input.iter().map(|a| &a.kind).collect();
    let mut sat = Saturation::new();
    let limits = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        ..Limits::default()
    };
    let retained_at_exit = Arc::new(AtomicUsize::new(0));
    let observed = retained_at_exit.clone();
    let out = admission_test::with_attempt(
        None,
        false,
        move |event| {
            if let admission_test::Attempt::Retained {
                bytes,
                root_bytes,
                pending,
                ..
            } = event
            {
                assert_eq!(pending, 0);
                observed.store(bytes - root_bytes, Ordering::Relaxed);
            }
        },
        || sat.classify(&refs, None, &limits).unwrap(),
    );
    assert!(out.stats.dl_rooted_restarts >= 1 && sat.index.is_some());
    assert_eq!(sat.excluded().len(), 1);
    let report_bytes = excluded_copy_bytes(&sat.excluded);
    assert!(report_bytes > 512 * 1024);
    let retained = sat.retained_bytes(0, 0, 0);
    let report = std::mem::take(&mut sat.excluded);
    let without_report = sat.retained_bytes(0, 0, 0);
    sat.excluded = report;

    let peak = std::cell::Cell::new(0);
    let build = |budget: &dyn Fn(usize) -> bool| {
        taxonomy::build(&sat, out.consistent, 1, false, &|| false, budget)
    };
    build(&|bytes| {
        peak.set(peak.get().max(bytes));
        true
    })
    .unwrap();
    let cap = without_report + report_bytes.max(peak.get());
    let paid = std::cell::Cell::new(retained);
    let tax_peak = std::cell::Cell::new(retained);
    let bounded = build(&Saturation::taxonomy_budget(
        retained,
        &paid,
        &tax_peak,
        Some(cap),
        None,
    ));
    assert!(
        matches!(bounded, Err(taxonomy::Halt::Memory(bytes)) if retained + bytes > cap),
        "taxonomy should refuse the overlap with the exclusion report"
    );
    assert_eq!(retained, without_report + report_bytes);
    assert_eq!(
        out.stats.memory,
        retained_at_exit.load(Ordering::Relaxed) + out.stats.taxonomy_memory
    );
}

#[test]
fn split_round4_ledger_covers_a_watermark_raising_fork() {
    use super::admission_test::{LedgerChargeSource, MergePhase, SplitAllocSite, with_merge_probe};

    // At the second fork the sibling's copied disequalities and the equality union
    // both extend the previous paid peak. Stop after those actual admissions.
    let suffix = "d".repeat(64 * 1024);
    let input = axioms(&format!(
        "SubClassOf(:X0 ObjectUnionOf(:B :X1))
         SubClassOf(:X1 ObjectUnionOf(:C :X2))
         SubClassOf(:X2 ObjectUnionOf(:D :X3))
         SubClassOf(:X3 ObjectUnionOf(:E :F))
         SubClassOf(:B ObjectOneOf(:b_{suffix}))
         SubClassOf(:C ObjectOneOf(:c_{suffix}))
         SubClassOf(:D ObjectOneOf(:d_{suffix}))
         SubClassOf(:E ObjectOneOf(:e_{suffix}))
         SubClassOf(:F ObjectOneOf(:f))
         ClassAssertion(:X0 :a_{suffix})
         ClassAssertion(:P :b_{suffix})
         ClassAssertion(:P :c_{suffix})
         ClassAssertion(:P :d_{suffix})
         ClassAssertion(:P :e_{suffix})
         ClassAssertion(:P :f)
         SubClassOf(:A ObjectOneOf(:a_{suffix}))",
    ));
    let refs: Vec<_> = input.iter().map(|axiom| &axiom.kind).collect();
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let record = events.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    let mut unions = 0;
    let result = with_merge_probe(
        move |phase| {
            record.lock().push(phase);
            if matches!(
                phase,
                MergePhase::SplitAlloc {
                    site: SplitAllocSite::ForkEqUnion,
                    admitted: true,
                    ..
                }
            ) {
                unions += 1;
                if unions == 2 {
                    stop.store(true, Ordering::Relaxed);
                }
            }
        },
        || {
            super::with_split_knobs(Some(0), Some(true), Some(8), Some(32), || {
                Saturation::new().classify(
                    &refs,
                    None,
                    &Limits {
                        threads: 1,
                        rooted: true,
                        rooted_merge: true,
                        rooted_eq_split: true,
                        dl_work: Some(1_000_000),
                        charge: Some(Arc::new(|_| true)),
                        cancelled: Some(Arc::new(move || cancelled.load(Ordering::Relaxed))),
                        ..Limits::default()
                    },
                )
            })
        },
    );
    assert!(matches!(result, Err(Error::Cancelled)));

    let events = events.lock();
    let (delta_paid, before, used, requested) = events
        .windows(4)
        .find_map(|events| match events {
            [
                MergePhase::LedgerCharge {
                    source: LedgerChargeSource::Delta,
                    paid_after,
                    requested: 1..,
                    admitted: true,
                    ..
                },
                MergePhase::SplitAlloc {
                    site: SplitAllocSite::ForkNeq,
                    admitted: true,
                    ..
                },
                MergePhase::LedgerCharge {
                    source: LedgerChargeSource::Cover,
                    paid_before,
                    used,
                    requested,
                    admitted: true,
                    ..
                },
                MergePhase::SplitAlloc {
                    site: SplitAllocSite::ForkEqUnion,
                    admitted: true,
                    ..
                },
            ] => Some((*paid_after, *paid_before, *used, *requested)),
            _ => None,
        })
        .expect("a paid ForkNeq addition followed immediately by a ForkEqUnion cover");
    assert_eq!(before, delta_paid);
    assert!(requested > 0);
    assert_eq!(requested, used - before);
}

#[test]
fn split_round4_ledger_covers_a_watermark_raising_merge_remap() {
    use super::admission_test::{LedgerChargeSource, MergePhase, with_merge_probe};

    // A derived union changes the short name in a surviving disequality to a long
    // representative. The replacement pair overlaps the old one, then the canonical
    // map takes another copy of the representative. Both phases extend the paid peak.
    let old = NamedIndividual::new("urn:test:z");
    let representative = NamedIndividual::new(format!("urn:test:a{}", "r".repeat(64 * 1024)));
    let mut merge = Merge::default();
    assert!(
        merge
            .union_named(&old, &representative, 0, &mut |_| true)
            .unwrap()
    );
    let mut neq = vec![(old, NamedIndividual::new("urn:test:zz"))];
    let mut neq_bytes = SplitFrame::neq_owned_bytes(&neq);
    let initial = merge.uf_bytes() + neq_bytes;
    let admitted = Arc::new(AtomicUsize::new(initial));
    let charged = Arc::new(AtomicUsize::new(0));
    let charge = charged.clone();
    let limits = Limits {
        charge: Some(Arc::new(move |more| {
            charge.fetch_add(more, Ordering::Relaxed);
            true
        })),
        ..Limits::default()
    };
    let ledger = SplitLedger {
        held_base: 0,
        live: AtomicUsize::new(initial),
        canon: AtomicUsize::new(0),
        peak: AtomicUsize::new(initial),
        admitted: &admitted,
        limits: &limits,
        lock: parking_lot::Mutex::new(()),
    };
    let events = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let record = events.clone();
    let remap_used = with_merge_probe(
        move |phase| record.lock().push(phase),
        || {
            let (collapsed, remap_used) =
                remap_split_neq(&merge, &mut neq, &mut neq_bytes, &ledger).unwrap();
            assert!(!collapsed);
            assert_eq!(neq[0].0, representative);
            let canon_base = ledger.base_with_canon(merge.uf_bytes()).unwrap();
            assert!(matches!(
                merge.build_canon(canon_base, &limits, &mut |used| ledger.cover(used)),
                Ok(Some(_))
            ));
            remap_used.unwrap()
        },
    );

    let events = events.lock();
    let [
        MergePhase::LedgerCharge {
            source: LedgerChargeSource::Delta,
            used,
            paid_before,
            paid_after,
            requested,
            admitted: true,
        },
        MergePhase::LedgerCharge {
            source: LedgerChargeSource::Cover,
            used: canon_used,
            paid_before: canon_before,
            paid_after: canon_after,
            requested: canon_requested,
            admitted: true,
        },
    ] = events.as_slice()
    else {
        panic!("expected the remap addition and canonical-map cover, got {events:?}");
    };
    assert_eq!(
        (*used, *paid_before, *paid_after),
        (remap_used, initial, remap_used)
    );
    assert_eq!(*requested, remap_used - initial);
    assert!(*requested > 0);
    assert_eq!(*canon_before, remap_used);
    assert_eq!(*canon_after, *canon_used);
    assert_eq!(*canon_requested, canon_used - canon_before);
    assert!(*canon_requested > 0);
    assert_eq!(charged.load(Ordering::Relaxed), canon_after - initial);
}

#[test]
fn split_round4_pre_plan_counts_retained_split_metadata() {
    let input = axioms("SubClassOf(:X ObjectUnionOf(:A :B))");
    let refs: Vec<_> = input.iter().map(|axiom| &axiom.kind).collect();
    let mut sat = Saturation::new();
    let out = sat
        .classify(
            &refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(out.complete && sat.index.is_some());

    // Rooted runs do not currently keep their DlRun. Seed the metadata on a kept
    // unrooted index to exercise the estimator independently of the keep policy.
    let before = sat.pre_plan_live_bytes(0);
    let run = sat.dl_run.as_mut().expect("retained Tier D state");
    assert_eq!(run.split_meta_bytes(), 0);
    run.const_rank = Box::new([0, 1, 2]);
    run.branch_neq = Box::new([(1, 2)]);
    let metadata = run.split_meta_bytes();
    assert_eq!(sat.pre_plan_live_bytes(0), before + metadata);
}

/// The ledger of the marker repair's table tests (item 1247). It refuses the admission it
/// is told to by its count, runs `hook` with its first admission, and fails a test that
/// admits under the table's lock.
struct MTabLedger<'a> {
    run: &'a DlRun,
    live: usize,
    admits: usize,
    refuse: Option<usize>,
    hook: Option<MTabHook<'a>>,
    /// when set, the tables that may be alive when a table's charge goes back unkept
    tables_at_refund: Option<isize>,
    refunds: usize,
}

/// What [`MTabLedger`] runs with its first admission.
type MTabHook<'a> = Box<dyn FnOnce(&DlRun) + 'a>;

impl<'a> MTabLedger<'a> {
    fn new(run: &'a DlRun) -> Self {
        Self {
            run,
            live: 0,
            admits: 0,
            refuse: None,
            hook: None,
            tables_at_refund: None,
            refunds: 0,
        }
    }
}

impl Sink for MTabLedger<'_> {
    fn send(&mut self, _: Ex, _: Concl) {}
    fn me(&mut self, _: Concl) {}
    fn dl(&self) -> &DlRun {
        self.run
    }
    fn admit_dl(&mut self, bytes: usize) -> bool {
        assert!(
            !self.run.mtab.get().is_some_and(|t| t.locked()),
            "an admission ran under the repair table's lock"
        );
        let n = self.admits;
        self.admits += 1;
        if let Some(hook) = self.hook.take() {
            hook(self.run);
        }
        if self.refuse == Some(n) {
            return false;
        }
        self.live += bytes;
        true
    }
    fn commit_dl(&mut self, admitted: usize, kept: usize) {
        assert!(kept <= admitted);
        assert!(
            !self.run.mtab.get().is_some_and(|t| t.locked()),
            "a commit ran under the repair table's lock"
        );
        if let Some(live) = self.tables_at_refund
            && admitted == super::dl_root::MTAB_BYTES
            && kept == 0
        {
            assert_eq!(
                super::dl_root::mtabs_live(),
                live,
                "a losing table was alive when its charge went back"
            );
            self.refunds += 1;
        }
        self.live -= admitted - kept;
    }
    fn credit_dl(&mut self, bytes: usize) {
        self.live = self.live.saturating_sub(bytes);
    }
    fn worked(&mut self, _: usize) {}
}

/// Item 1247: a rooted run without the marker repair makes no table for the repair, and
/// its contexts name no slot, while a run with it gives each context that holds repair
/// state its own slot of the run's table, which the run's ledger holds.
#[test]
fn the_marker_repair_state_is_in_a_table_that_only_a_run_with_the_repair_makes() {
    let (ix, _, _) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let slot = |st: &State| super::dl_rules::dl(st).map_or(0, |x| x.mslot);
    for mode in [0, MREPAIR_ON | super::dl_rules::MREPAIR_CF] {
        let mut run = DlRun::new(false, Default::default());
        run.mrepair = mode;
        let mut sink = MTabLedger::new(&run);
        let mut root_st = State::default();
        super::dl_root::init(&ix, &mut root_st, &mut sink);
        let mut ctxs: Vec<State> = (0..3).map(|_| State::default()).collect();
        for (i, st) in ctxs.iter_mut().enumerate() {
            let me = 100 + i as Ex;
            assert!(super::dl_root::redge(&mut root_st, me, 0, &mut sink));
            if mode != 0 {
                assert!(super::dl_root::rreach(
                    &ix,
                    me,
                    st,
                    super::index::TOP,
                    &mut sink
                ));
            }
        }
        let named: Vec<u32> = std::iter::once(&root_st)
            .chain(&ctxs)
            .map(slot)
            .filter(|&s| s != 0)
            .collect();
        if mode == 0 {
            assert!(
                run.mtab.get().is_none(),
                "a run without the repair made the table"
            );
            assert!(
                named.is_empty(),
                "a context names a slot without the repair"
            );
            assert_eq!(run.mtab_bytes(), 0);
        } else {
            let tab = run.mtab.get().expect("the repair makes its table");
            assert_eq!(named.len(), 4, "the root and the three reached contexts");
            let mut distinct = named.clone();
            distinct.sort_unstable();
            distinct.dedup();
            assert_eq!(distinct, named, "each context has a slot of its own");
            assert_eq!(
                tab.len(),
                named.len(),
                "a slot for each context that holds one"
            );
            assert!(run.mtab_bytes() > 0 && sink.live >= run.mtab_bytes());
        }
    }
}

/// A context's repair state is taken out of its slot once at a time (item 1247), so that a
/// handler that reached its context's rules again while it held the state would panic in
/// the tests rather than lose a change.
#[test]
#[should_panic(expected = "taken once at a time")]
fn a_context_s_repair_state_is_taken_once_at_a_time() {
    let (ix, _, _) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let mut run = DlRun::new(false, Default::default());
    run.mrepair = MREPAIR_ON;
    let mut sink = MTabLedger::new(&run);
    let mut st = State::default();
    super::dl_root::init(&ix, &mut st, &mut sink);
    let x = super::dl_rules::dl(&st).expect("dl state");
    let _held = super::dl_root::mrep_get(x, &sink);
    let _again = super::dl_root::mrep_get(x, &sink);
}

/// A run with the marker repair, for the table tests of item 1247.
fn mtab_run() -> DlRun {
    let mut run = DlRun::new(false, Default::default());
    run.mrepair = MREPAIR_ON | super::dl_rules::MREPAIR_CF;
    run
}

/// Item 1247: a refused admission of the run's table, of a context's first slot or of the
/// growth of the slots leaves the context naming no slot and the ledger holding exactly
/// what the table holds, and the table grows past its first allocation with each slot
/// charged exactly.
#[test]
fn the_marker_repair_table_admits_each_slot_exactly_and_keeps_nothing_it_refused() {
    use super::dl_root::{MTAB_BYTES, mrep_admit};
    use super::dl_rules::ds;
    let run = mtab_run();
    let mut sink = MTabLedger::new(&run);
    let mut ctxs: Vec<State> = (0..12).map(|_| State::default()).collect();

    // a refusal of the table itself makes nothing
    sink.refuse = Some(0);
    assert!(mrep_admit(ds(&mut ctxs[0]), &mut sink).is_none());
    assert!(run.mtab.get().is_none(), "a refused table was made");
    assert_eq!(ds(&mut ctxs[0]).mslot, 0);
    assert_eq!(sink.live, 0);

    // a refusal of the first slot keeps the table, empty, and its charge
    sink.refuse = Some(2);
    assert!(mrep_admit(ds(&mut ctxs[0]), &mut sink).is_none());
    let tab = run.mtab.get().expect("the table stays once it is admitted");
    assert_eq!(ds(&mut ctxs[0]).mslot, 0);
    assert_eq!(tab.len(), 0);
    assert_eq!(tab.bytes(), MTAB_BYTES);
    assert_eq!(sink.live, tab.bytes());

    // four slots fill the first allocation, each charged exactly
    sink.refuse = None;
    for (i, st) in ctxs[..4].iter_mut().enumerate() {
        let held = mrep_admit(ds(st), &mut sink).expect("admitted");
        drop(held);
        assert_eq!(ds(st).mslot, i as u32 + 1);
        assert_eq!(sink.live, tab.bytes(), "the ledger after slot {}", i + 1);
    }
    let full = tab.bytes();

    // a refusal of the growth past the first allocation keeps the four slots as they were
    sink.refuse = Some(sink.admits);
    assert!(mrep_admit(ds(&mut ctxs[4]), &mut sink).is_none());
    assert_eq!(ds(&mut ctxs[4]).mslot, 0);
    assert_eq!(tab.len(), 4);
    assert_eq!(tab.bytes(), full);
    assert_eq!(sink.live, full);

    // the slots grow twice past it, with the ledger exact after every slot
    sink.refuse = None;
    for (i, st) in ctxs.iter_mut().enumerate().skip(4) {
        let held = mrep_admit(ds(st), &mut sink).expect("admitted");
        drop(held);
        assert_eq!(ds(st).mslot, i as u32 + 1);
        assert_eq!(sink.live, tab.bytes(), "the ledger after slot {}", i + 1);
    }
    assert_eq!(tab.len(), ctxs.len());
    assert!(
        tab.bytes()
            >= MTAB_BYTES
                + 16 * std::mem::size_of::<Option<Box<super::dl_root::MRep>>>()
                + ctxs.len() * std::mem::size_of::<super::dl_root::MRep>(),
        "twelve slots grew past eight"
    );

    // a context that has its slot takes it again with nothing more charged
    let before = sink.live;
    drop(mrep_admit(ds(&mut ctxs[0]), &mut sink).expect("its slot"));
    assert_eq!(sink.live, before);
    assert_eq!(run.mtab_bytes(), sink.live);
}

/// Item 1247: two workers that both find the run without its table make one each, and the
/// one that loses the race frees its table and gives its charge back, so that the two
/// contexts share the winner's table and the two ledgers together hold exactly its bytes.
#[test]
fn the_worker_that_loses_the_marker_repair_table_race_gives_its_charge_back() {
    use super::dl_root::{MTAB_BYTES, mrep_admit};
    use super::dl_rules::ds;
    let run = mtab_run();
    let mut winner_st = State::default();
    let winner_live = std::cell::Cell::new(0);
    let mut loser = MTabLedger::new(&run);
    {
        let (winner_st, winner_live) = (&mut winner_st, &winner_live);
        loser.hook = Some(Box::new(move |run: &DlRun| {
            assert!(run.mtab.get().is_none(), "the loser found the table");
            let mut winner = MTabLedger::new(run);
            drop(mrep_admit(ds(winner_st), &mut winner).expect("the winner admits"));
            winner_live.set(winner.live);
        }));
    }
    // only the winner's table may be alive when the loser gives its table's charge back
    let tables_before = super::dl_root::mtabs_live();
    loser.tables_at_refund = Some(tables_before + 1);
    let mut loser_st = State::default();
    drop(mrep_admit(ds(&mut loser_st), &mut loser).expect("the loser admits"));
    assert_eq!(loser.refunds, 1, "the loser gave its table's charge back");
    assert_eq!(super::dl_root::mtabs_live(), tables_before + 1);
    let loser_live = loser.live;
    drop(loser);
    let tab = run.mtab.get().expect("the winner's table");
    assert_eq!(tab.len(), 2, "both contexts have a slot of the one table");
    assert_eq!(ds(&mut winner_st).mslot, 1);
    assert_eq!(ds(&mut loser_st).mslot, 2);
    assert_eq!(
        loser_live,
        std::mem::size_of::<super::dl_root::MRep>(),
        "the loser keeps its state's charge and none of its table's"
    );
    assert!(winner_live.get() >= MTAB_BYTES);
    assert_eq!(winner_live.get() + loser_live, tab.bytes());
}

/// A context's repair state is read only while it is in its slot (item 1247), so that a
/// handler that read it while it held it would panic in the tests.
#[test]
#[should_panic(expected = "read while it is in its slot")]
fn a_context_s_repair_state_is_read_only_in_its_slot() {
    use super::dl_root::{mrep_admit, mrep_peek};
    use super::dl_rules::ds;
    let run = mtab_run();
    let mut sink = MTabLedger::new(&run);
    let mut st = State::default();
    let _held = mrep_admit(ds(&mut st), &mut sink).expect("admitted");
    let x = super::dl_rules::dl(&st).expect("dl state");
    mrep_peek(x, &sink, |_| ());
}
