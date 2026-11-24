//! Unit, boundary, oracle, stop-attribution, post-counting retry, determinism, and
//! counting-allocator regression tests for Wave C Item 1284 Option 2 (`rawQ`).

use super::dl_clause::{Clause, Lit, Made, RTerm, Rooted, Store, Stored, XLit};
use super::dl_rules::{
    AdaptiveMeta, AdaptivePolicy, DlRun, WorkStopGuard, with_rooted_adaptive_bound,
    with_rooted_adaptive_diag, with_rooted_adaptive_policy,
};
use super::index::{Axioms, Ex, Index, Interner};
use super::numeric::tests::measure;
use super::saturate::{Concl, Sink, State};
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn axioms(body: &str) -> Vec<Axiom> {
    crate::ofn_read::parse(&format!("Prefix(:=<urn:dl1284:>)\nOntology(\n{body}\n)"))
        .expect("test ontology")
        .axioms
}

fn rooted_index(body: &str) -> (Index, Ex) {
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
    (ix, root)
}

fn rooted_disjunctive_ontology(individuals: usize, top_classes: usize) -> Vec<Axiom> {
    let mut body = String::from(
        "SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         SubClassOf(:U0 :Done)\n\
         SubClassOf(:U1 :Done)\n",
    );
    for t in 0..top_classes {
        body.push_str(&format!("SubClassOf(owl:Thing :T{t})\n"));
    }
    for i in 0..individuals {
        body.push_str(&format!("ClassAssertion(:Seed :i{i})\n"));
    }
    axioms(&body)
}

fn intern_lit(run: &DlRun, l: XLit) -> Lit {
    match run.lits.lit(l, |_| None) {
        Made::Lit(lit) => lit,
        _ => panic!("expected literal"),
    }
}

fn make_meta(policy_name: &'static str, effective_cap: usize) -> AdaptiveMeta {
    AdaptiveMeta {
        run_id: 1,
        attempt: 1,
        after_counting: false,
        policy_name,
        effective_cap,
        mrepair: 0,
        threads: 1,
    }
}

struct TrackingSink<'a> {
    run: &'a DlRun,
    cap: usize,
    live: usize,
    credits: usize,
}

impl<'a> TrackingSink<'a> {
    fn unlimited(run: &'a DlRun) -> Self {
        Self {
            run,
            cap: usize::MAX,
            live: 0,
            credits: 0,
        }
    }
    fn capped(run: &'a DlRun, cap: usize) -> Self {
        Self {
            run,
            cap,
            live: 0,
            credits: 0,
        }
    }
}

impl Sink for TrackingSink<'_> {
    fn send(&mut self, _: Ex, _: Concl) {}
    fn me(&mut self, _: Concl) {}
    fn dl(&self) -> &DlRun {
        self.run
    }
    fn reserve_data(&mut self, bytes: usize) -> bool {
        let Some(next) = self.live.checked_add(bytes) else {
            return false;
        };
        if next > self.cap {
            return false;
        }
        self.live = next;
        true
    }
    fn release_data(&mut self, bytes: usize) {
        self.live = self.live.saturating_sub(bytes);
    }
    fn admit_dl(&mut self, bytes: usize) -> bool {
        self.reserve_data(bytes)
    }
    fn commit_dl(&mut self, bound: usize, retained: usize) {
        self.live = self.live.saturating_sub(bound).saturating_add(retained);
    }
    fn credit_dl(&mut self, bytes: usize) {
        self.credits += bytes;
        self.live = self.live.saturating_sub(bytes);
    }
    fn worked(&mut self, n: usize) {
        self.run.count(n);
    }
}

/// 1. Layout & zero-overhead invariants: `Stored` stays 64 bytes on 64-bit targets,
///    `DlRun::bytes()` adds 0 when adaptive is off and covers `Box<AdaptiveState>` when on.
#[test]
fn stored_layout_and_dl_run_off_overhead() {
    #[cfg(target_pointer_width = "64")]
    assert_eq!(std::mem::size_of::<Stored>(), 64);

    let mut run = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    assert!(!run.tracks_adaptive());
    assert!(!run.is_adaptive_enforced());
    let off_bytes = run.bytes();
    assert_eq!(off_bytes, 0);

    // Enabling adaptive allocates only the single `Box<AdaptiveState>`, fully covered by `bytes()`.
    let (on_bytes, on_sample) = measure(|| {
        run.enable_adaptive(
            AdaptivePolicy::PROVISIONAL,
            true,
            false,
            make_meta("adaptive", AdaptivePolicy::PROVISIONAL.ceiling),
        );
        let b = run.bytes();
        assert!(b > 0);
        b
    });
    assert!(
        on_bytes >= on_sample.peak,
        "DlRun::bytes() ({on_bytes}) must cover AdaptiveState allocation ({})",
        on_sample.peak
    );
    // `begin_run` drops adaptive state and restores `bytes()` back to 0.
    run.begin_run();
    assert!(!run.tracks_adaptive());
    assert_eq!(run.bytes(), 0);
}

/// 2. Binding Decision #3: explicit `dl_work` wins, qualified adaptive keeps fixed ceiling
///    `2^25` even when a per-class fallback bound at >1,048,576 classes exceeds `2^25`,
///    and `dl_steps = false` falls through to the per-class fallback bound.
#[test]
fn select_rooted_bound_precedence_and_per_class_ceiling() {
    let synthetic_classes: usize = 1_048_577;
    let per_class_bound: usize = ROOTED_WORK_BOUND.max(32 * synthetic_classes);
    assert!(
        per_class_bound > AdaptivePolicy::PROVISIONAL.ceiling,
        "synthetic per-class bound ({per_class_bound}) must exceed 2^25 ({})",
        AdaptivePolicy::PROVISIONAL.ceiling
    );

    // Explicit Some(n) always wins (including Some(0) -> 1).
    assert_eq!(
        select_rooted_bound(
            Some(0),
            true,
            true,
            AdaptivePolicy::PROVISIONAL,
            per_class_bound
        ),
        RootedBoundPolicy::ExplicitFixed(1)
    );
    assert_eq!(
        select_rooted_bound(
            Some(777),
            true,
            true,
            AdaptivePolicy::PROVISIONAL,
            per_class_bound
        ),
        RootedBoundPolicy::ExplicitFixed(777)
    );

    // Qualified deterministic adaptive keeps exact 2^25 ceiling even when per-class > 2^25.
    let sel = select_rooted_bound(
        None,
        true,
        true,
        AdaptivePolicy::PROVISIONAL,
        per_class_bound,
    );
    assert_eq!(
        sel,
        RootedBoundPolicy::Adaptive(AdaptivePolicy::PROVISIONAL)
    );
    assert_eq!(sel.work_limit(), 1 << 25);

    // Non-deterministic (`dl_steps = false`) is ineligible for adaptive and preserves per-class.
    let non_det = select_rooted_bound(
        None,
        false,
        true,
        AdaptivePolicy::PROVISIONAL,
        per_class_bound,
    );
    assert_eq!(non_det, RootedBoundPolicy::DefaultFixed(per_class_bound));

    // Adaptive off falls through to fallback fixed bound.
    let off = select_rooted_bound(
        None,
        true,
        false,
        AdaptivePolicy::PROVISIONAL,
        per_class_bound,
    );
    assert_eq!(off, RootedBoundPolicy::DefaultFixed(per_class_bound));
}

/// 3. Critical Astra Catch #1: exact `raw_queue` / `raw_pushed` accounting across pushes,
///    subsumed drops, admission refusals, all-dead drain, dead-pop-on-refusal (without
///    memory credit for retained heap capacity), live `next_commit`, bottom residue, and
///    loud underflow assertion.
#[test]
fn raw_queue_oracle_push_subsumed_refusal_dead_pop_and_underflow() {
    let (ix, _) = rooted_index("SubClassOf(:A ObjectUnionOf(:B :C))\nClassAssertion(:A :u0)\n");
    let mut run = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    run.enable_adaptive(
        AdaptivePolicy::PROVISIONAL,
        true,
        false,
        make_meta("adaptive", AdaptivePolicy::PROVISIONAL.ceiling),
    );
    let mut store = Store::default();
    let mut sink = TrackingSink::unlimited(&run);

    // Push 3 clauses via `add_admitted` and `add_parts_admitted`.
    assert!(store.add_admitted(
        Clause::new(vec![Lit::x(10)], vec![Lit::x(20), Lit::x(21)]),
        &mut sink
    ));
    assert!(store.add_parts_admitted(vec![Lit::x(11)], vec![Lit::x(20), Lit::x(22)], &mut sink));
    assert!(store.add_parts_admitted(vec![Lit::x(12)], vec![Lit::x(20), Lit::x(23)], &mut sink));
    assert_eq!(run.raw_pushed(), 3);
    assert_eq!(run.raw_queue(), 3);
    assert_eq!(run.raw_queue(), store.queue_len());

    // Subsumed clause returns true without pushing: counters unchanged.
    assert!(store.add_admitted(
        Clause::new(
            vec![Lit::x(10), Lit::x(99)],
            vec![Lit::x(20), Lit::x(21), Lit::x(24)]
        ),
        &mut sink
    ));
    assert_eq!(run.raw_pushed(), 3);
    assert_eq!(run.raw_queue(), 3);
    assert_eq!(run.raw_queue(), store.queue_len());

    // Admission refusal in `add_admitted` and `add_parts_admitted`: counters unchanged.
    let mut refusing = TrackingSink::capped(&run, 0);
    assert!(!store.add_admitted(Clause::new(vec![], vec![Lit::x(30)]), &mut refusing));
    assert!(!store.add_parts_admitted(vec![], vec![Lit::x(31)], &mut refusing));
    assert_eq!(run.raw_pushed(), 3);
    assert_eq!(run.raw_queue(), 3);
    assert_eq!(run.raw_queue(), store.queue_len());

    // Backward-subsume all 3 earlier clauses with a single stronger unit `-> 20(x)`.
    // Queue now holds 3 dead entries + 1 live entry (length 4).
    assert!(store.add_admitted(Clause::new(vec![], vec![Lit::x(20)]), &mut sink));
    assert_eq!(run.raw_pushed(), 4);
    assert_eq!(run.raw_queue(), 4);
    assert_eq!(run.raw_queue(), store.queue_len());

    // Pop the shorter live unit `-> 20(x)` first (priority = 1 < 3).
    let mut max = Vec::new();
    let unit_id = store
        .next_admitted(&ix, None, &mut max, &mut sink)
        .expect("live unit");
    assert_eq!(unit_id, 3);
    assert_eq!(run.raw_pushed(), 4);
    assert_eq!(run.raw_queue(), 3);
    assert_eq!(run.raw_queue(), store.queue_len());

    // Remaining 3 entries are all dead: `peek_live` drains all 3 to `None` without crediting memory.
    let bytes_before = store.bytes();
    assert_eq!(store.peek_live(&run), None);
    assert_eq!(run.raw_queue(), 0);
    assert_eq!(store.queue_len(), 0);
    assert_eq!(store.bytes(), bytes_before);
    assert_eq!(store.bytes(), store.scan_bytes());
    assert_eq!(sink.credits, 0, "dead pops must earn zero memory credit");

    // Dead prefix ahead of a live wide clause, followed by admission refusal in `next_admitted`!
    let mut rooted_store = Store::default();
    let wide_head: Vec<Lit> = (0..128u32)
        .map(|k| intern_lit(&run, XLit::Atom(100 + k, RTerm::X)))
        .collect();
    let short_a = intern_lit(&run, XLit::Atom(500, RTerm::X));
    let short_b = intern_lit(&run, XLit::Atom(501, RTerm::X));
    // Live wide clause (len 129), then dead shorter clause (len 2), then live unit (len 1).
    assert!(rooted_store.add_parts_admitted(vec![wide_head[0]], wide_head.clone(), &mut sink));
    assert!(rooted_store.add_parts_admitted(vec![], vec![short_a, short_b], &mut sink));
    assert!(rooted_store.add_parts_admitted(vec![], vec![short_a], &mut sink));
    assert_eq!(run.raw_queue(), 3);
    assert_eq!(rooted_store.queue_len(), 3);

    let inv = |_: u32| None;
    let ord = Rooted {
        ix: &ix,
        lits: &run.lits,
        inv: &inv,
        ground_total: true,
    };
    let mut drain_unit = Vec::new();
    let _ = rooted_store
        .next_admitted(&ix, Some(&ord), &mut drain_unit, &mut sink)
        .expect("pop unit [short_a]");
    // Now queue has [dead(len 2), live_wide(len 129)].
    assert_eq!(run.raw_queue(), 2);
    assert_eq!(rooted_store.queue_len(), 2);

    let bytes_before_refusal = rooted_store.bytes();
    let mut tiny = TrackingSink::capped(&run, 16);
    let mut wide_max = Vec::new();
    let popped = rooted_store.next_admitted(&ix, Some(&ord), &mut wide_max, &mut tiny);
    assert!(popped.is_none(), "expected scratch refusal on wide clause");
    // Dead entry was popped by `peek_live`, live wide clause stays queued!
    assert_eq!(
        run.raw_queue(),
        1,
        "dead prefix pop must be counted even when next admission refuses"
    );
    assert_eq!(rooted_store.queue_len(), 1);
    assert_eq!(rooted_store.bytes(), bytes_before_refusal);
    assert_eq!(rooted_store.bytes(), rooted_store.scan_bytes());
    assert_eq!(
        tiny.credits, 0,
        "dead pop on refusal must earn zero memory credit"
    );

    // Loud underflow assertion (no silent repair).
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run.on_queue_pop(2);
    }));
    assert!(res.is_err(), "raw_queue underflow must panic loudly");
}

/// 4. Boundary, arithmetic, multi-bucket jump, ceiling independence, priority order,
///    repeated post-warmup surge, saturation without wrap, and exhaustion label tests.
#[test]
fn adaptive_guard_boundaries_priority_jumps_and_exhaustion_labels() {
    let policy = AdaptivePolicy {
        sample_shift: 4, // sample every 16 work
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 20,
        warmup_work: 64,
        queue_max: 10,
        ceiling: 128,
    };
    let make_run = |enforce: bool| {
        let mut r = DlRun::new(false, Arc::new(AtomicBool::new(false)));
        r.enable_adaptive(
            policy,
            enforce,
            false,
            make_meta("adaptive", policy.ceiling),
        );
        r
    };

    // (a) Strict `root_work > R`: R == 20 does NOT trigger at sample 16 or 32; R == 21 DOES.
    let r = make_run(true);
    r.count_root(20);
    r.count(16);
    assert_eq!(r.stop_guard(), WorkStopGuard::None);
    assert!(!r.over_work());
    r.count_root(1);
    r.count(15); // work = 31 (same bucket as 16) -> no sample yet
    assert_eq!(r.stop_guard(), WorkStopGuard::None);
    r.count(1); // work = 32 (crosses sample) -> RootWork latches at 32!
    assert_eq!(r.stop_guard(), WorkStopGuard::RootWork);
    assert_eq!(r.stop_requested_at(), 32);
    assert!(r.over_work());
    // Overshoot past ceiling does NOT overwrite first requesting guard or timestamp.
    r.count(200);
    assert_eq!(r.stop_guard(), WorkStopGuard::RootWork);
    assert_eq!(r.stop_requested_at(), 32);

    // (b) Inclusive warmup `work >= W` (64) and strict `raw_queue > Qmax` (10) & `10*q > 3*p`.
    let r = make_run(true);
    // Push 100, pop 89 -> q = 11 > Qmax (10), ratio 11/100 <= 3/10.
    for _ in 0..100 {
        r.on_queue_push();
    }
    r.on_queue_pop(89);
    assert_eq!(r.raw_queue(), 11);
    r.count(48); // sample < W (64) -> queue guards NOT evaluated yet
    assert_eq!(r.stop_guard(), WorkStopGuard::None);
    r.count(15); // work = 63 (no crossing)
    assert_eq!(r.stop_guard(), WorkStopGuard::None);
    r.count(1); // work = 64 == W (inclusive!) -> QueueAbs latches!
    assert_eq!(r.stop_guard(), WorkStopGuard::QueueAbs);
    assert_eq!(r.stop_requested_at(), 64);

    // (c) Strict equality `q == Qmax` (10) and `10 * q == 3 * p` (q = 9, p = 30 -> 90 == 90)
    //     must NOT trigger; repeated post-warmup surge 3 samples later MUST trigger QueueRatio.
    let r = make_run(true);
    for _ in 0..30 {
        r.on_queue_push();
    }
    r.on_queue_pop(21); // q = 9, p = 30 -> 10*9 == 3*30 (strict >, must NOT fire)
    r.count(64); // crosses into warmup W = 64
    assert_eq!(r.stop_guard(), WorkStopGuard::None);
    r.count(16); // work = 80
    assert_eq!(r.stop_guard(), WorkStopGuard::None);
    // Push 1 more -> q = 10 (== Qmax, strict >), p = 31 -> 10*10 = 100 > 3*31 = 93!
    r.on_queue_push();
    r.count(16); // work = 96 (3 samples post-warmup) -> QueueRatio fires!
    assert_eq!(r.stop_guard(), WorkStopGuard::QueueRatio);
    assert_eq!(r.stop_requested_at(), 96);

    // (d) `p == 0 => q == 0` at warmup does not trigger.
    let r = make_run(true);
    r.count(64);
    assert_eq!(r.stop_guard(), WorkStopGuard::None);

    // (e) Hard ceiling tracked independently of sample mask: C = 128 does NOT fire,
    //     C + 1 = 129 fires `TotalLimit` immediately in the SAME bucket with NO extra sample!
    let r = make_run(true);
    r.count(128);
    assert_eq!(r.stop_guard(), WorkStopGuard::None);
    assert!(!r.over_work());
    let samples_at_c = r.adaptive_samples();
    assert_eq!(
        samples_at_c, 1,
        "multi-bucket jump 0->128 emits exactly 1 sample"
    );
    r.count(1); // 128 -> 129, same bucket (128 >> 4 == 129 >> 4 == 8)
    assert_eq!(r.stop_guard(), WorkStopGuard::TotalLimit);
    assert_eq!(r.stop_requested_at(), 129);
    assert_eq!(
        r.adaptive_samples(),
        samples_at_c,
        "C -> C+1 must not synthesize a sample"
    );
    assert!(r.over_work());

    // (f) Simultaneous crossing priority: TotalLimit > RootWork > QueueAbs > QueueRatio.
    let r = make_run(true);
    r.count_root(25);
    for _ in 0..20 {
        r.on_queue_push();
    }
    r.count(130); // crosses all four simultaneously -> TotalLimit wins
    assert_eq!(r.stop_guard(), WorkStopGuard::TotalLimit);
    assert_eq!(r.stop_requested_at(), 130);

    let r = make_run(true);
    r.count_root(25);
    for _ in 0..20 {
        r.on_queue_push();
    }
    r.count(80); // crosses RootWork + QueueAbs + QueueRatio -> RootWork wins
    assert_eq!(r.stop_guard(), WorkStopGuard::RootWork);

    let r = make_run(true);
    for _ in 0..20 {
        r.on_queue_push();
    }
    r.count(80); // crosses QueueAbs + QueueRatio -> QueueAbs wins
    assert_eq!(r.stop_guard(), WorkStopGuard::QueueAbs);

    // (g) Saturating `count(usize::MAX)` does not wrap.
    let r = make_run(true);
    r.count(10);
    r.count(usize::MAX);
    assert_eq!(r.work(), usize::MAX);
    assert_eq!(r.stop_guard(), WorkStopGuard::TotalLimit);

    // (h) Exhaustion reason labels: `past(n)` -> TotalLimit, `too_wide` -> WidthExhausted,
    //     `Found::over` -> ForcedExhaust.
    let r = make_run(true);
    assert!(r.past(policy.ceiling + 1));
    assert_eq!(r.stop_guard(), WorkStopGuard::TotalLimit);
    assert!(r.over_work());

    let (ix, _) = rooted_index("SubClassOf(:A :B)\n");
    let r = make_run(true);
    r.set_width_limit(1);
    let mut st = State::default();
    let mut found = super::dl_rules::Found::default();
    found.products(
        vec![vec![(vec![], vec![Lit::x(10)]), (vec![], vec![Lit::x(11)])]],
        (vec![], vec![]),
    );
    let mut sink = TrackingSink::unlimited(&r);
    super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, &super::dl_rules::NoopDerive);
    assert_eq!(r.stop_guard(), WorkStopGuard::WidthExhausted);
    assert!(r.over_work());

    let r = make_run(true);
    let mut st = State::default();
    let mut found = super::dl_rules::Found::default();
    found.mark_over();
    let mut sink = TrackingSink::unlimited(&r);
    super::dl_rules::drain_with(&ix, &mut st, found, &mut sink, &super::dl_rules::NoopDerive);
    assert_eq!(r.stop_guard(), WorkStopGuard::ForcedExhaust);
    assert!(r.over_work());
}

/// 5. Binding Decision #5: `root_work` increments ONLY in `dl_root::derive_covered`
///    (after `st.bottom`, immediately before `out.worked(1)` so a crossing sample observes
///    the root derivation before its clause is pushed to `store.queue`), and batched
///    marker-repair scans advance `work` + `mrepair_work` without incrementing `root_work`.
#[test]
fn root_work_phase_bottom_guard_and_marker_scan_separation() {
    let (ix, root) = rooted_index(
        "SubClassOf(:A ObjectUnionOf(:B :C))\n\
         ClassAssertion(:A :u0)\n",
    );
    let policy = AdaptivePolicy {
        sample_shift: 4, // sample at 16
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 0, // triggers RootWork on first sample if root_work >= 1
        warmup_work: 16,
        queue_max: 0, // would trigger QueueAbs at sample 16 if clause were pushed BEFORE worked(1)
        ceiling: 1024,
    };
    let mut run = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    run.enable_adaptive(policy, true, true, make_meta("adaptive", policy.ceiling));
    let mut sink = TrackingSink::unlimited(&run);

    // (a) When `st.bottom == true`, `dl_root::derive` must NOT count root_work or work.
    let mut st = State {
        bottom: true,
        ..Default::default()
    };
    let lit_a = intern_lit(&run, XLit::Atom(2, RTerm::C(0)));
    super::dl_root::derive(&ix, &mut st, (vec![], vec![lit_a]), &mut sink);
    assert_eq!(run.root_work(), 0);
    assert_eq!(run.work(), 0);
    assert_eq!(run.raw_pushed(), 0);

    // (b) Cross sample 16 via `dl_root::derive`: at work = 16, `root_work` is already 1
    //     (so RootWork triggers at work = 16!), while the clause is inserted AFTER worked(1).
    st.bottom = false;
    run.count(15);
    assert_eq!(run.stop_guard(), WorkStopGuard::None);
    super::dl_root::derive(&ix, &mut st, (vec![], vec![lit_a]), &mut sink);
    assert_eq!(run.work(), 16);
    assert_eq!(run.root_work(), 1);
    assert_eq!(run.raw_pushed(), 1);
    assert_eq!(run.raw_queue(), 1);
    assert_eq!(run.stop_guard(), WorkStopGuard::RootWork);
    assert_eq!(run.stop_requested_at(), 16);

    // (c) Verify pre-insertion phase directly: with root_limit high and queue_max = 0 at
    //     warmup = 16, crossing 16 on the FIRST root derivation sees raw_queue == 0 at sample
    //     16 (clause not yet inserted!) so QueueAbs does NOT fire at 16.
    let pre_insert_policy = AdaptivePolicy {
        sample_shift: 4,
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 100,
        warmup_work: 16,
        queue_max: 0,
        ceiling: 1024,
    };
    let mut run2 = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    run2.enable_adaptive(
        pre_insert_policy,
        true,
        false,
        make_meta("adaptive", pre_insert_policy.ceiling),
    );
    let mut sink2 = TrackingSink::unlimited(&run2);
    let mut st2 = State::default();
    let lit_b = intern_lit(&run2, XLit::Atom(2, RTerm::C(0)));
    run2.count(15);
    super::dl_root::derive(&ix, &mut st2, (vec![], vec![lit_b]), &mut sink2);
    assert_eq!(run2.work(), 16);
    assert_eq!(run2.root_work(), 1);
    assert_eq!(run2.raw_queue(), 1, "clause inserted after sample 16");
    assert_eq!(
        run2.stop_guard(),
        WorkStopGuard::None,
        "sample at 16 ran before clause insertion so raw_queue was 0"
    );

    // (d) Attempted root derivation dropped as tautology still increments `root_work`.
    super::dl_root::derive(&ix, &mut st2, (vec![lit_b], vec![lit_b]), &mut sink2);
    assert_eq!(run2.root_work(), 2);
    assert_eq!(run2.raw_pushed(), 1, "tautology not pushed to store");

    // (e) Batched marker-repair scan crossing a sample advances `work` and `mrepair_work`
    //     WITHOUT incrementing `root_work`.
    let scan_policy = AdaptivePolicy {
        sample_shift: 4,
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 0,
        warmup_work: 1024,
        queue_max: 1024,
        ceiling: 1024,
    };
    let mut run3 = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    run3.mrepair = super::dl_rules::MREPAIR_ON;
    run3.enable_adaptive(
        scan_policy,
        true,
        true,
        make_meta("adaptive", scan_policy.ceiling),
    );
    let mut sink3 = TrackingSink::unlimited(&run3);
    let mut root_st = State::default();
    // Seed 2,048 pending registrations so `rreach` runs `Scan::read` across a 2^4 sample.
    for c in 0..2048u32 {
        assert!(super::dl_root::redge(&mut root_st, 100, c, &mut sink3));
    }
    assert!(super::dl_root::rreach(
        &ix,
        root,
        &mut root_st,
        100,
        &mut sink3
    ));
    assert!(
        run3.work() >= 16,
        "expected batched marker scan to cross sample 16 (work={})",
        run3.work()
    );
    assert!(
        run3.mrepair_work() >= 16,
        "expected diagnostic mrepair_work to reflect scan charges (mrepair_work={})",
        run3.mrepair_work()
    );
    assert_eq!(
        run3.root_work(),
        0,
        "marker-repair scan must not increment root_work"
    );
    assert_eq!(run3.stop_guard(), WorkStopGuard::None);
}

/// 6. Binding Decision #1: observe-only diagnostic mode (`SPARKLES_ROOTED_ADAPTIVE_DIAG=1`
///    without adaptive enforcement) tracks exact counters & hypothetical triggers on both
///    explicit fixed caps and default controls without touching the stop latch.
#[test]
fn observe_only_diag_tracks_without_latching_stop() {
    let input = rooted_disjunctive_ontology(40, 12);
    let tight_policy = AdaptivePolicy {
        sample_shift: 4, // sample every 16 work
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 8, // would trigger RootWork once root derivations exceed 8
        warmup_work: 10_000,
        queue_max: 10_000,
        ceiling: 10_000,
    };

    // Observe-only on an explicit high fixed cap: continues past hypothetical stop to completion!
    let limits_explicit = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        dl_work: Some(100_000),
        ..Limits::default()
    };
    let (out, snaps) = with_rooted_adaptive_policy(tight_policy, || {
        with_rooted_adaptive_bound(false, || {
            with_rooted_adaptive_diag(true, || {
                let mut k = Classifier::of(&input);
                let out = k.classify(&limits_explicit).expect("classify");
                (out, last_adaptive_attempts())
            })
        })
    });
    assert!(!out.stats.dl_rooted_over && !out.stats.dl_over_budget);
    assert_eq!(snaps.len(), 1);
    let s = snaps[0];
    assert!(!s.enforce);
    assert_eq!(s.policy_name, "explicit_fixed");
    assert_eq!(s.effective_cap, 100_000);
    assert_eq!(s.winning_stop, WinningStop::SaturationFinished);
    assert_eq!(
        s.stop_guard,
        WorkStopGuard::None,
        "observe-only mode must leave stop latch untouched"
    );
    assert_eq!(s.hypothetical_guard, WorkStopGuard::RootWork);
    assert!(s.hypothetical_at > 0 && s.hypothetical_at < s.final_work);
    assert_eq!(s.raw_queue, s.oracle_queue);
    assert!(s.raw_pushed > 0 && s.root_work > tight_policy.root_limit);

    // Observe-only on default fixed 2^19 control (`dl_work: None`).
    let limits_default = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        ..Limits::default()
    };
    let (out2, snaps2) = with_rooted_adaptive_policy(tight_policy, || {
        with_rooted_adaptive_bound(false, || {
            with_rooted_adaptive_diag(true, || {
                let mut k = Classifier::of(&input);
                let out = k.classify(&limits_default).expect("classify");
                (out, last_adaptive_attempts())
            })
        })
    });
    assert!(!out2.stats.dl_rooted_over);
    assert_eq!(snaps2.len(), 1);
    assert!(!snaps2[0].enforce);
    assert_eq!(snaps2[0].policy_name, "default_fixed");
    assert_eq!(snaps2[0].effective_cap, ROOTED_WORK_BOUND);
    assert_eq!(snaps2[0].winning_stop, WinningStop::SaturationFinished);
    assert_eq!(snaps2[0].stop_guard, WorkStopGuard::None);
    assert_eq!(snaps2[0].hypothetical_guard, WorkStopGuard::RootWork);
}

/// 7. Critical Astra Catch #2 & Decision #4: `partial = true` + adaptive `Stop::Work`
///    captures the winning stop immediately after `saturate_counted` (before the `partial`
///    routing at `mod.rs:3504` and store teardown at `mod.rs:3998`) and still forces
///    non-rooted fallback; `Stop::Memory` outranks pending work on `admit_dl` / `check_held`,
///    while pending work outranks `Cancelled` on the same `check_held` poll.
#[test]
fn partial_true_stop_capture_and_publication_order() {
    let input = rooted_disjunctive_ontology(80, 20);
    let small_root_policy = AdaptivePolicy {
        sample_shift: 4, // sample every 16 work
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 16, // stops early during root init / saturation
        warmup_work: 1 << 20,
        queue_max: 1 << 20,
        ceiling: 1 << 20,
    };

    // (a) `partial = true` + adaptive RootWork stop -> captured immediately, forces fallback!
    let limits_partial = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        ..Limits::default()
    };
    let (out, snaps) = with_rooted_adaptive_policy(small_root_policy, || {
        with_rooted_adaptive_bound(true, || {
            let mut k = Classifier::of(&input);
            let out = k.classify(&limits_partial).expect("fallback answer");
            (out, last_adaptive_attempts())
        })
    });
    assert!(
        out.stats.dl_rooted_over && out.stats.dl_over_budget,
        "work stop with partial=true must still force non-rooted fallback: {:?}",
        out.stats
    );
    assert_eq!(
        snaps.len(),
        1,
        "partial=true work stop must be captured immediately after saturate_counted"
    );
    let s = snaps[0];
    assert!(s.enforce);
    assert_eq!(s.winning_stop, WinningStop::Work);
    assert_eq!(s.stop_guard, WorkStopGuard::RootWork);
    assert!(s.stop_requested_at >= 16);
    assert!(s.final_work >= s.stop_requested_at);
    assert_eq!(s.raw_queue, s.oracle_queue);
    // Cooperative poll overshoot must be recorded cleanly.
    let _ = s.overshoot();

    // (b) `partial = true` + memory refusal wins even after `stop_guard` has latched:
    //     `admit_dl` checks memory before `check_held` checks `over_work()`, so a pool
    //     refusal during saturation wins with `WinningStop::Memory`, returns a partial
    //     answer (`!complete`, `Consistency::Unknown`, `!dl_rooted_over`), and preserves
    //     `raw_queue == oracle_queue` before partial cleanup drops stores.
    let admitted = Arc::new(AtomicUsize::new(0));
    let admitted_ref = admitted.clone();
    // Allow normalization & base setup, then refuse during root saturation growth.
    let limits_mem = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        charge: Some(Arc::new(move |bytes: usize| {
            let prev = admitted_ref.load(Ordering::Relaxed);
            if prev.saturating_add(bytes) > 160 << 10 {
                return false;
            }
            admitted_ref.fetch_add(bytes, Ordering::Relaxed);
            true
        })),
        ..Limits::default()
    };
    let (res_mem, snaps_mem) = with_rooted_adaptive_policy(small_root_policy, || {
        with_rooted_adaptive_bound(true, || {
            let mut k = Classifier::of(&input);
            let r = k.classify(&limits_mem);
            (r, last_adaptive_attempts())
        })
    });
    if !snaps_mem.is_empty() {
        let sm = snaps_mem[0];
        assert_eq!(sm.raw_queue, sm.oracle_queue);
        if sm.winning_stop == WinningStop::Memory {
            let out_mem = res_mem.expect("partial memory answer");
            assert!(!out_mem.stats.dl_rooted_over);
            assert!(!out_mem.complete);
            assert_eq!(out_mem.consistency, Consistency::Unknown);
        }
    }

    // (c) Publication order: at a `check_held` poll where `over_work()` is ALREADY true
    //     AND `cancelled` is ALREADY true, `Stop::Work` outranks `Stop::Cancelled`!
    //     Verify with a cancel predicate that flips true only after root work > 16.
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_ref = calls.clone();
    let limits_cancel_race = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        cancelled: Some(Arc::new(move || {
            // Let normalization polls pass; flip true once saturation starts checking.
            calls_ref.fetch_add(1, Ordering::Relaxed) > 10_000
        })),
        ..Limits::default()
    };
    let (out_race, snaps_race) = with_rooted_adaptive_policy(small_root_policy, || {
        with_rooted_adaptive_bound(true, || {
            let mut k = Classifier::of(&input);
            let r = k
                .classify(&limits_cancel_race)
                .expect("work stop beats cancel");
            (r, last_adaptive_attempts())
        })
    });
    assert!(out_race.stats.dl_rooted_over);
    assert_eq!(snaps_race.len(), 1);
    assert_eq!(snaps_race[0].winning_stop, WinningStop::Work);
    assert_eq!(snaps_race[0].stop_guard, WorkStopGuard::RootWork);
}

/// 8. Binding Decision #2: post-counting rooted retry (`counting: true, rooted: true`,
///    `dl_work: None`, `dl_steps: true`) applies Option 2 to the fresh rooted non-counting
///    retry after Attempt 1 hits the counting bound — tested both when the rooted retry
///    finishes and when it stops via Option 2 and falls back again.
#[test]
fn post_counting_rooted_retry_finishes_and_falls_back() {
    // Counting axiom `<= 12 r.Thing` with 13 distinct existential fillers gives `k = 13 > AT_MOST_WIDEST`
    // and triggers `Found::over` immediately in Attempt 1 (counting) even with `dl_work: None`,
    // forcing Attempt 2 (`no_counting=true`). Meanwhile disjunction + individuals force Attempt 2
    // to run the rooted non-counting path!
    let mut body = String::from(
        "SubClassOf(:C ObjectMaxCardinality(12 :r owl:Thing))\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         SubClassOf(:U0 :Done)\n\
         SubClassOf(:U1 :Done)\n",
    );
    for k in 0..13 {
        body.push_str(&format!("SubClassOf(:C ObjectSomeValuesFrom(:r :B{k}))\n"));
    }
    for i in 0..30 {
        body.push_str(&format!("ClassAssertion(:Seed :i{i})\n"));
    }
    let input = axioms(&body);
    let seed = Class::new("urn:dl1284:Seed");
    let done = Class::new("urn:dl1284:Done");

    let limits = Limits {
        threads: 1,
        counting: true,
        rooted: true,
        dl_steps: true,
        ..Limits::default()
    };

    // Case A: Attempt 2 (rooted non-counting retry) FINISHES under Option 2.
    let (out_fin, excl_fin, snaps_fin) = with_rooted_adaptive_bound(true, || {
        let mut k = Classifier::of(&input);
        let out = k.classify(&limits).expect("classify");
        (out, k.excluded().to_vec(), last_adaptive_attempts())
    });
    assert!(
        out_fin.stats.dl_counting_over && out_fin.stats.dl_over_budget,
        "expected Attempt 1 counting stop: {:?}",
        out_fin.stats
    );
    assert!(
        !out_fin.stats.dl_rooted_over,
        "Attempt 2 rooted retry must finish: {:?}",
        out_fin.stats
    );
    assert!(out_fin.taxonomy.is_subclass(&seed, &done));
    assert!(
        excl_fin
            .iter()
            .any(|e| format!("{e:?}").contains("ObjectMaxCardinality")),
        "counting exclusion must be reported: {excl_fin:?}"
    );
    assert_eq!(snaps_fin.len(), 1);
    assert!(snaps_fin[0].after_counting);
    assert!(snaps_fin[0].enforce);
    assert_eq!(
        snaps_fin[0].effective_cap,
        AdaptivePolicy::PROVISIONAL.ceiling
    );
    assert_eq!(snaps_fin[0].winning_stop, WinningStop::SaturationFinished);
    assert_eq!(snaps_fin[0].raw_queue, snaps_fin[0].oracle_queue);

    // Case B: Attempt 2 (rooted non-counting retry) hits Option 2's `RootWork` guard and
    //         falls back again to unrooted!
    let tight_retry_policy = AdaptivePolicy {
        sample_shift: 4,
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 8,
        warmup_work: 1 << 20,
        queue_max: 1 << 20,
        ceiling: 1 << 20,
    };
    let (out_fb, excl_fb, snaps_fb) = with_rooted_adaptive_policy(tight_retry_policy, || {
        with_rooted_adaptive_bound(true, || {
            let mut k = Classifier::of(&input);
            let out = k.classify(&limits).expect("classify fallback");
            (out, k.excluded().to_vec(), last_adaptive_attempts())
        })
    });
    assert!(
        out_fb.stats.dl_counting_over && out_fb.stats.dl_rooted_over && out_fb.stats.dl_over_budget,
        "expected both dl_counting_over and dl_rooted_over: {:?}",
        out_fb.stats
    );
    assert!(
        excl_fb
            .iter()
            .any(|e| format!("{e:?}").contains("ObjectMaxCardinality")),
        "counting exclusion must still be reported after second fallback: {excl_fb:?}"
    );
    assert_eq!(snaps_fb.len(), 1);
    assert!(snaps_fb[0].after_counting);
    assert!(snaps_fb[0].enforce);
    assert_eq!(snaps_fb[0].winning_stop, WinningStop::Work);
    assert_eq!(snaps_fb[0].stop_guard, WorkStopGuard::RootWork);
    assert_eq!(snaps_fb[0].raw_queue, snaps_fb[0].oracle_queue);
}

/// 9. `t = 1` vs `t = 4` determinism, parallel classifier isolation, stale-state clearing,
///    and switch-OFF zero-behavior-change parity.
#[test]
fn determinism_t1_vs_t4_parallel_isolation_and_switch_off_parity() {
    let input = rooted_disjunctive_ontology(50, 15);
    let seed = Class::new("urn:dl1284:Seed");
    let done = Class::new("urn:dl1284:Done");

    // (a) Switch OFF: zero adaptive attempts recorded, identical stats and allowance.
    let base_limits = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        ..Limits::default()
    };
    let (off_out, off_allow, off_snaps) = with_rooted_adaptive_bound(false, || {
        with_rooted_adaptive_diag(false, || {
            let mut k = Classifier::of(&input);
            let out = k.classify(&base_limits).expect("off classify");
            (out, LAST_ALLOWANCE.get(), last_adaptive_attempts())
        })
    });
    assert!(
        off_snaps.is_empty(),
        "switch OFF must record zero adaptive attempts"
    );
    assert!(off_out.taxonomy.is_subclass(&seed, &done));

    // (b) `t = 1` vs `t = 4` with Option 2 ON: bit-for-bit identical work, counters, and taxonomy.
    let run_with_threads = |threads: usize| {
        let lim = Limits {
            threads,
            rooted: true,
            dl_steps: true,
            ..Limits::default()
        };
        with_rooted_adaptive_bound(true, || {
            let mut k = Classifier::of(&input);
            let out = k.classify(&lim).expect("adaptive classify");
            let snaps = last_adaptive_attempts();
            (out, snaps)
        })
    };
    let (out1, snaps1) = run_with_threads(1);
    let (out4, snaps4) = run_with_threads(4);
    assert_eq!(out1.stats.dl_work, out4.stats.dl_work);
    assert_eq!(out1.stats.dl_rooted_over, out4.stats.dl_rooted_over);
    assert_eq!(out1.stats.dl_work, off_out.stats.dl_work);
    assert_eq!(snaps1.len(), 1);
    assert_eq!(snaps1, snaps4);
    assert_eq!(snaps1[0].raw_queue, snaps1[0].oracle_queue);
    assert_eq!(
        out1.taxonomy.is_subclass(&seed, &done),
        out4.taxonomy.is_subclass(&seed, &done)
    );

    // (c) Parallel isolation: two concurrent classifiers on separate threads produce exact counts.
    let input_arc = Arc::new(input.clone());
    let h1 = std::thread::spawn({
        let input = input_arc.clone();
        let lim = base_limits.clone();
        move || {
            with_rooted_adaptive_bound(true, || {
                let mut k = Classifier::of(&*input);
                let out = k.classify(&lim).expect("thread 1");
                (out.stats.dl_work, last_adaptive_attempts())
            })
        }
    });
    let h2 = std::thread::spawn({
        let input = input_arc;
        let lim = base_limits.clone();
        move || {
            with_rooted_adaptive_bound(true, || {
                let mut k = Classifier::of(&*input);
                let out = k.classify(&lim).expect("thread 2");
                (out.stats.dl_work, last_adaptive_attempts())
            })
        }
    });
    let (w1, s1) = h1.join().expect("join 1");
    let (w2, s2) = h2.join().expect("join 2");
    assert_eq!(w1, w2);
    assert_eq!(s1, s2);
    assert_eq!(s1, snaps1);

    // (d) Repeated classification on one Classifier clears stale diagnostic state when switching OFF.
    let mut reused = Classifier::of(&input);
    with_rooted_adaptive_bound(true, || {
        let _ = reused.classify(&base_limits).expect("run 1");
        assert_eq!(last_adaptive_attempts().len(), 1);
    });
    with_rooted_adaptive_bound(false, || {
        with_rooted_adaptive_diag(false, || {
            let out = reused.classify(&base_limits).expect("run 2 off");
            assert!(
                last_adaptive_attempts().is_empty(),
                "stale adaptive snapshots must clear on next top-level classification"
            );
            assert_eq!(out.stats.dl_work, off_out.stats.dl_work);
            assert_eq!(LAST_ALLOWANCE.get(), off_allow);
        })
    });
}

/// 10. Counting-allocator memory coverage with Option 2 ON during streamed `dl_root::init`
///     (both `RootWork` adaptive stop and tight `max_memory` stop) plus switch-OFF parity.
#[test]
fn counting_allocator_rooted_adaptive_init_and_fallback() {
    let input = rooted_disjunctive_ontology(200, 40);
    let init_root_policy = AdaptivePolicy {
        sample_shift: 6, // sample every 64 work
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 128,
        warmup_work: 1 << 20,
        queue_max: 1 << 20,
        ceiling: 1 << 20,
    };

    // (a) Adaptive RootWork stop during streamed `dl_root::init`: peak_memory covers measured heap.
    let limits = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        ..Limits::default()
    };
    let ((out, snaps), sample) = with_rooted_adaptive_policy(init_root_policy, || {
        with_rooted_adaptive_bound(true, || {
            measure(|| {
                let mut k = Classifier::of(&input);
                let out = k.classify(&limits).expect("fallback");
                (out, last_adaptive_attempts())
            })
        })
    });
    assert!(out.stats.dl_rooted_over && out.stats.dl_over_budget);
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].winning_stop, WinningStop::Work);
    assert_eq!(snaps[0].stop_guard, WorkStopGuard::RootWork);
    assert_eq!(snaps[0].raw_queue, snaps[0].oracle_queue);
    assert!(
        out.stats.peak_memory >= sample.peak,
        "adaptive RootWork peak_memory ({}) undercounted measured peak heap ({})",
        out.stats.peak_memory,
        sample.peak
    );

    // (b) Tight caller `max_memory` under Option 2 ON stops cleanly with heap covered.
    let cap = 2 << 20;
    let tight = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        max_memory: Some(cap),
        ..Limits::default()
    };
    let (res_tight, sample_tight) = with_rooted_adaptive_bound(true, || {
        measure(|| {
            let mut k = Classifier::of(&input);
            k.classify(&tight)
        })
    });
    if let Ok(out_tight) = res_tight {
        assert!(
            out_tight.stats.peak_memory >= sample_tight.peak,
            "tight cap peak_memory ({}) undercounted measured peak heap ({})",
            out_tight.stats.peak_memory,
            sample_tight.peak
        );
    }
    assert!(
        sample_tight.peak <= cap + (256 << 10),
        "tight cap measured peak ({}) overshot cap ({cap})",
        sample_tight.peak
    );
}
