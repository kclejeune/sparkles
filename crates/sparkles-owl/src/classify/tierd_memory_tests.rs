//! Fail-first counting-allocator, refusal/cleanup, and deterministic-allowance regression
//! tests for roadmap item 1291 (Tier D memory accounting).

use super::dl_clause::{
    BEFORE_LIT_REVALIDATE, Clause, Lit, Lits, Made, RTerm, Rooted, Store, XLit, hash_table_bytes,
};
use super::dl_rules::{Deriver, DlRun, Found, MREPAIR_ON, NoopDerive};
use super::index::{Axioms, Ex, Index, Interner};
use super::numeric::tests::measure;
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

    // 2. Rooted `next_admitted` with a dead queue entry ahead of a wide 256-literal clause:
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
    // Add a weaker clause that gets backward-subsumed so a dead entry sits in `queue`.
    let mut weaker = wide_head.clone();
    let Made::Lit(extra) = lits.lit(XLit::Atom(999, RTerm::C(1)), inv) else {
        panic!()
    };
    weaker.push(extra);
    assert!(rooted_store.add_parts_admitted(vec![], weaker, &mut unlimited));
    assert!(rooted_store.add_parts_admitted(vec![], vec![extra], &mut unlimited));
    assert!(rooted_store.clauses.iter().any(|s| s.dead));
    let bytes_before = rooted_store.bytes();

    let ord = Rooted {
        ix: &ix,
        lits: &lits,
        inv: &inv,
        ground_total: true,
    };
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
        max.capacity(),
        0,
        "max must not allocate when scratch is refused"
    );
    assert_eq!(sample.live, 0, "refused rooted next must retain zero heap");
    let _ = exact_needed;
}

/// Non-ground `fold` with `slack_covered > 0`, `FOLD_PRUNE` streaming transition, and wide
/// unions under spare capacity (on both non-ground and ground paths) must keep every live
/// buffer charged through its last use and stop immediately on refusal.
#[test]
fn fold_covered_slack_prune_streaming_and_wide_spare_capacity() {
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

    // A. `slack_covered > 0` with large wide unions (`union_now` >> `slack_covered`):
    // reservation must include `union_now` on both growth and prune paths and cover heap.
    {
        let mut st = State::default();
        let _ = super::dl_rules::ds(&mut st);
        let wide = |base: Ex| (Vec::new(), (base..base + 256).map(Lit::x).collect());
        let list_a: Vec<_> = (0..16u32).map(|i| wide(100 + i * 300)).collect();
        let list_b: Vec<_> = (0..8u32).map(|i| wide(10_000 + i * 300)).collect();
        let mut found = Found::default();
        found.products(vec![list_a, list_b], (Vec::new(), Vec::new()));
        let slack = found.fold_slack();
        assert!(slack > 0);
        let mut sink = TrackSink {
            run: &run,
            cap: usize::MAX,
            live: slack, // caller pre-reserved `slack`
            peak: slack,
            stopped: false,
        };
        let (_, sample) = measure(|| {
            super::dl_rules::drain_covered(&ix, &mut st, found, &mut sink, false, slack);
        });
        assert!(
            sink.peak >= sample.peak,
            "covered fold peak ({}) undercounted heap ({})",
            sink.peak,
            sample.peak
        );
        // After drain, only `slack` plus retained Store growth remains in `sink.live`.
        let store_bytes = super::dl_rules::dl(&st).map_or(0, |d| d.store.bytes());
        assert_eq!(
            sink.live,
            slack + store_bytes,
            "fold must release all temporary reservations"
        );

        // Tight cap covering `slack` + `held` + 64 KiB (not enough for 128 wide unions) must
        // refuse before allocating all unions even with `slack_covered > 0`.
        let mut st2 = State::default();
        let _ = super::dl_rules::ds(&mut st2);
        let list_a2: Vec<_> = (0..16u32).map(|i| wide(100 + i * 300)).collect();
        let list_b2: Vec<_> = (0..8u32).map(|i| wide(10_000 + i * 300)).collect();
        let mut found2 = Found::default();
        found2.products(vec![list_a2, list_b2], (Vec::new(), Vec::new()));
        let held2 = found2.held_bytes();
        let mut tight = TrackSink {
            run: &run,
            cap: slack + held2 + (64 << 10),
            live: slack,
            peak: slack,
            stopped: false,
        };
        super::dl_rules::drain_covered(&ix, &mut st2, found2, &mut tight, false, slack);
        assert!(tight.stopped, "tight cap must refuse wide covered fold");
        assert!(st2.interrupted);
        assert_eq!(tight.live, slack);
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
    let dl_bytes = super::dl_rules::dl(&st).expect("dl state").bytes();
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
        let before_dl = super::dl_rules::dl(&st).unwrap().bytes();
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
        let after_dl = super::dl_rules::dl(&st).unwrap().bytes();
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
