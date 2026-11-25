//! Unit, boundary, oracle, stop-attribution, post-counting retry, determinism, and
//! counting-allocator regression tests for Wave C Item 1284 Option 2 (`rawQ`).

use super::dl_clause::{Clause, Lit, Made, RTerm, Rooted, Store, Stored, XLit};
use super::dl_rules::{
    AdaptiveMeta, AdaptivePolicy, DlRun, WorkStopGuard, adaptive_alloc_count,
    reset_adaptive_alloc_count, with_on_adaptive_latch, with_on_adaptive_sample,
    with_rooted_adaptive_bound, with_rooted_adaptive_diag, with_rooted_adaptive_policy,
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

    // Explicit `kill_head` unit-subsumption kill leaves a dead resident entry in `queue`
    // (0 push/pop change), and `peek_live` drains it to decrement `raw_queue`.
    let mut unit_kill_store = Store::default();
    let before_kill_q = run.raw_queue();
    let before_kill_p = run.raw_pushed();
    assert!(unit_kill_store.add_admitted(
        Clause::new(vec![Lit::x(70)], vec![Lit::x(77), Lit::x(78)]),
        &mut sink
    ));
    assert_eq!(run.raw_pushed(), before_kill_p + 1);
    assert_eq!(run.raw_queue(), before_kill_q + 1);
    assert_eq!(unit_kill_store.queue_len(), 1);
    unit_kill_store.kill_head(Lit::x(77));
    assert_eq!(
        run.raw_queue(),
        before_kill_q + 1,
        "kill_head marks dead without popping the heap"
    );
    assert_eq!(unit_kill_store.queue_len(), 1);
    assert_eq!(unit_kill_store.peek_live(&run), None);
    assert_eq!(run.raw_queue(), before_kill_q);
    assert_eq!(unit_kill_store.queue_len(), 0);

    // Bottom context with a nonempty resident heap keeps its residue until teardown and
    // skips root_work/work/push on subsequent derivations into the already-bottom state.
    let mut bottom_st = State::default();
    let lit_r0 = intern_lit(&run, XLit::Atom(2, RTerm::C(0)));
    let lit_r1 = intern_lit(&run, XLit::Atom(3, RTerm::C(0)));
    super::dl_root::derive(&ix, &mut bottom_st, (vec![], vec![lit_r0]), &mut sink);
    super::dl_root::derive(&ix, &mut bottom_st, (vec![], vec![lit_r1]), &mut sink);
    let q_with_residue = run.raw_queue();
    let p_with_residue = run.raw_pushed();
    let w_with_residue = run.work();
    let rw_with_residue = run.root_work();
    let dl_q = bottom_st
        .extra
        .as_ref()
        .and_then(|x| x.dl.as_ref())
        .map_or(0, |d| d.store.queue_len());
    assert_eq!(dl_q, 2, "expected 2 resident queued clauses before bottom");
    bottom_st.bottom = true;
    let lit_r2 = intern_lit(&run, XLit::Atom(4, RTerm::C(0)));
    super::dl_root::derive(&ix, &mut bottom_st, (vec![], vec![lit_r2]), &mut sink);
    assert_eq!(run.work(), w_with_residue);
    assert_eq!(run.root_work(), rw_with_residue);
    assert_eq!(run.raw_pushed(), p_with_residue);
    assert_eq!(run.raw_queue(), q_with_residue);
    assert_eq!(
        bottom_st
            .extra
            .as_ref()
            .and_then(|x| x.dl.as_ref())
            .map_or(0, |d| d.store.queue_len()),
        2,
        "bottom context must retain resident heap residue until teardown"
    );

    // Loud underflow assertion (no silent repair).
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run.on_queue_pop(q_with_residue + 1);
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

    // Observe-only on an explicit fixed cap exceeding `policy.ceiling`: hypothetical
    // `TotalLimit` latches without touching `stop_guard`, and saturation still finishes.
    let low_ceiling_policy = AdaptivePolicy {
        sample_shift: 4,
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 100_000,
        warmup_work: 100_000,
        queue_max: 100_000,
        ceiling: 64,
    };
    let (out3, snaps3) = with_rooted_adaptive_policy(low_ceiling_policy, || {
        with_rooted_adaptive_bound(false, || {
            with_rooted_adaptive_diag(true, || {
                let mut k = Classifier::of(&input);
                let out = k.classify(&limits_explicit).expect("classify");
                (out, last_adaptive_attempts())
            })
        })
    });
    assert!(!out3.stats.dl_rooted_over && !out3.stats.dl_over_budget);
    assert_eq!(snaps3.len(), 1);
    assert!(!snaps3[0].enforce);
    assert_eq!(snaps3[0].policy_name, "explicit_fixed");
    assert_eq!(snaps3[0].effective_cap, 100_000);
    assert_eq!(snaps3[0].winning_stop, WinningStop::SaturationFinished);
    assert_eq!(snaps3[0].stop_guard, WorkStopGuard::None);
    assert_eq!(snaps3[0].hypothetical_guard, WorkStopGuard::TotalLimit);
    assert!(snaps3[0].hypothetical_at > low_ceiling_policy.ceiling);
    assert!(snaps3[0].final_work > snaps3[0].hypothetical_at);
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

    // (b) Strict non-vacuous `partial = true` + memory refusal winning AFTER `stop_guard`
    //     latched: refuse the very next positive saturation debit once `RootWork` latches,
    //     so `admit_dl` publishes `STOP_MEMORY` before the next `check_held` poll sees
    //     `over_work()`. Unconditionally verify both the pending guard and Memory winner.
    let latched = Arc::new(AtomicBool::new(false));
    let refused = Arc::new(AtomicBool::new(false));
    let latched_hook = latched.clone();
    let latched_charge = latched.clone();
    let refused_charge = refused.clone();
    let limits_mem = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        charge: Some(Arc::new(move |_bytes: usize| {
            if latched_charge.load(Ordering::SeqCst) && !refused_charge.swap(true, Ordering::SeqCst)
            {
                return false;
            }
            true
        })),
        ..Limits::default()
    };
    let (res_mem, snaps_mem) = with_rooted_adaptive_policy(small_root_policy, || {
        with_rooted_adaptive_bound(true, || {
            with_on_adaptive_latch(
                move |guard, _| {
                    if guard == WorkStopGuard::RootWork {
                        latched_hook.store(true, Ordering::SeqCst);
                    }
                },
                || {
                    let mut k = Classifier::of(&input);
                    let r = k.classify(&limits_mem);
                    (r, last_adaptive_attempts())
                },
            )
        })
    });
    assert!(
        latched.load(Ordering::SeqCst),
        "RootWork guard must latch before memory refusal"
    );
    assert!(
        refused.load(Ordering::SeqCst),
        "pool debit must be refused after RootWork latched"
    );
    assert_eq!(snaps_mem.len(), 1);
    let sm = snaps_mem[0];
    assert_eq!(sm.stop_guard, WorkStopGuard::RootWork);
    assert_eq!(sm.winning_stop, WinningStop::Memory);
    assert_eq!(sm.raw_queue, sm.oracle_queue);
    let out_mem = res_mem.expect("partial memory answer");
    assert!(!out_mem.stats.dl_rooted_over && !out_mem.stats.dl_over_budget);
    assert!(!out_mem.complete && !out_mem.complete_for_taken);
    assert_eq!(out_mem.consistency, Consistency::Unknown);

    // (b2) Neon Blocker #1 / Sol r2 #2 regression: `partial = true` + `entangled == true`
    //      + `Stop::Memory` winning AFTER `stop_guard` latched. Run under
    //      `admission_test::with_attempt(None, true, ...)` (enabling `DisjWithIndividuals`
    //      just like `partial_pending_messages_count_individual_marking_seeds_once`) so
    //      `:Witness ⊑ {:w}` sets `totals.merged == true`, and observe `Attempt::Marking`
    //      with `seeds > 0` to PROVE the individual-marking loop actually ran where the
    //      stale adaptive `stop_guard` used to abort the partial answer into Horn fallback.
    let mut ent_body = String::from(
        "SubClassOf(:Witness ObjectOneOf(:w))\n\
         SubClassOf(:Witness :Tag)\n\
         SubClassOf(:Reaches ObjectSomeValuesFrom(:r :Witness))\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         SubClassOf(:U0 :Done)\n\
         SubClassOf(:U1 :Done)\n",
    );
    for t in 0..20 {
        ent_body.push_str(&format!("SubClassOf(owl:Thing :T{t})\n"));
    }
    for i in 0..80 {
        ent_body.push_str(&format!("ClassAssertion(:Seed :i{i})\n"));
    }
    let ent_input = axioms(&ent_body);
    let ent_kinds: Vec<&AxiomKind> = ent_input.iter().map(|a| &a.kind).collect();
    let ent_latched = Arc::new(AtomicBool::new(false));
    let ent_refused = Arc::new(AtomicBool::new(false));
    let ent_latched_hook = ent_latched.clone();
    let ent_latched_charge = ent_latched.clone();
    let ent_refused_charge = ent_refused.clone();
    let marking_seeds: Arc<parking_lot::Mutex<Vec<usize>>> = Arc::default();
    let marking_seeds_obs = marking_seeds.clone();
    let limits_ent_mem = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        charge: Some(Arc::new(move |_bytes: usize| {
            if ent_latched_charge.load(Ordering::SeqCst)
                && !ent_refused_charge.swap(true, Ordering::SeqCst)
            {
                return false;
            }
            true
        })),
        ..Limits::default()
    };
    let mut ent_sat = Saturation::new();
    let (res_ent, snaps_ent) = with_rooted_adaptive_policy(small_root_policy, || {
        with_rooted_adaptive_bound(true, || {
            with_on_adaptive_latch(
                move |guard, _| {
                    if guard == WorkStopGuard::RootWork {
                        ent_latched_hook.store(true, Ordering::SeqCst);
                    }
                },
                || {
                    admission_test::with_attempt(
                        None,
                        true,
                        move |event| {
                            if let admission_test::Attempt::Marking { seeds, .. } = event {
                                marking_seeds_obs.lock().push(seeds);
                            }
                        },
                        || {
                            let r = ent_sat.classify(&ent_kinds, None, &limits_ent_mem);
                            (r, last_adaptive_attempts())
                        },
                    )
                },
            )
        })
    });
    assert!(ent_latched.load(Ordering::SeqCst) && ent_refused.load(Ordering::SeqCst));
    assert!(ent_sat.dl_rooted, "must stay on the rooted partial run");
    let observed_marking = marking_seeds.lock().clone();
    assert!(
        observed_marking.iter().any(|&seeds| seeds > 0),
        "partial entangled rooted run must actually enter the individual-marking round with nonempty seeds: {observed_marking:?}"
    );
    assert_eq!(snaps_ent.len(), 1);
    assert_eq!(snaps_ent[0].stop_guard, WorkStopGuard::RootWork);
    assert_eq!(snaps_ent[0].winning_stop, WinningStop::Memory);
    assert_eq!(snaps_ent[0].raw_queue, snaps_ent[0].oracle_queue);
    let out_ent = res_ent.expect("partial entangled answer must not abort on stale stop_guard");
    assert!(
        !out_ent.stats.dl_rooted_over && !out_ent.stats.dl_over_budget,
        "entangled partial answer must not discard partial result into Horn fallback: {:?}",
        out_ent.stats
    );
    assert!(!out_ent.complete && !out_ent.complete_for_taken);
    assert_eq!(out_ent.consistency, Consistency::Unknown);

    // (c) Strict non-vacuous publication order: arm `cancelled = true` deterministically
    //     immediately when `RootWork` latches (BEFORE the decisive `check_held` poll).
    //     At that `check_held` poll both `over_work()` and `cancelled()` are true, so
    //     `Stop::Work` MUST win on the rooted attempt; the subsequent unrooted fallback then
    //     observes `cancelled() == true` and returns `Err(Error::Cancelled)`.
    let cancel_armed = Arc::new(AtomicBool::new(false));
    let cancel_hook = cancel_armed.clone();
    let cancel_pred = cancel_armed.clone();
    let limits_cancel_race = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        cancelled: Some(Arc::new(move || cancel_pred.load(Ordering::SeqCst))),
        ..Limits::default()
    };
    let (res_race, snaps_race) = with_rooted_adaptive_policy(small_root_policy, || {
        with_rooted_adaptive_bound(true, || {
            with_on_adaptive_latch(
                move |guard, _| {
                    if guard == WorkStopGuard::RootWork {
                        cancel_hook.store(true, Ordering::SeqCst);
                    }
                },
                || {
                    let mut k = Classifier::of(&input);
                    let r = k.classify(&limits_cancel_race);
                    (r, last_adaptive_attempts())
                },
            )
        })
    });
    assert!(
        cancel_armed.load(Ordering::SeqCst),
        "cancel predicate must be armed before the decisive check_held poll"
    );
    assert!(
        matches!(res_race, Err(Error::Cancelled)),
        "after Work wins on the rooted attempt, the unrooted retry must observe cancelled=true: {res_race:?}"
    );
    assert_eq!(snaps_race.len(), 1);
    assert_eq!(snaps_race[0].stop_guard, WorkStopGuard::RootWork);
    assert_eq!(snaps_race[0].winning_stop, WinningStop::Work);
    assert_eq!(snaps_race[0].raw_queue, snaps_race[0].oracle_queue);

    // (d) Companion arm: earlier cancellation BEFORE any adaptive guard latches wins with
    //     `WinningStop::Cancelled` and returns the partial answer (`!complete`,
    //     `Consistency::Unknown`, `!dl_rooted_over`).
    let high_root_policy = AdaptivePolicy {
        sample_shift: 4,
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 100_000,
        warmup_work: 1 << 20,
        queue_max: 1 << 20,
        ceiling: 1 << 20,
    };
    let early_cancel = Arc::new(AtomicBool::new(false));
    let early_cancel_hook = early_cancel.clone();
    let early_cancel_pred = early_cancel.clone();
    let limits_early_cancel = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        cancelled: Some(Arc::new(move || early_cancel_pred.load(Ordering::SeqCst))),
        ..Limits::default()
    };
    let (res_early, snaps_early) = with_rooted_adaptive_policy(high_root_policy, || {
        with_rooted_adaptive_bound(true, || {
            with_on_adaptive_sample(
                move |_work, guard| {
                    if guard == WorkStopGuard::None {
                        early_cancel_hook.store(true, Ordering::SeqCst);
                    }
                },
                || {
                    let mut k = Classifier::of(&input);
                    let r = k.classify(&limits_early_cancel);
                    (r, last_adaptive_attempts())
                },
            )
        })
    });
    assert!(
        early_cancel.load(Ordering::SeqCst),
        "early cancel must fire at a passing sample before any guard latches"
    );
    assert_eq!(snaps_early.len(), 1);
    assert_eq!(snaps_early[0].stop_guard, WorkStopGuard::None);
    assert_eq!(snaps_early[0].winning_stop, WinningStop::Cancelled);
    assert_eq!(snaps_early[0].raw_queue, snaps_early[0].oracle_queue);
    let out_early = res_early.expect("partial cancelled answer");
    assert!(!out_early.stats.dl_rooted_over && !out_early.stats.dl_over_budget);
    assert!(!out_early.complete && !out_early.complete_for_taken);
    assert_eq!(out_early.consistency, Consistency::Unknown);
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

    // Compare Case B's fallback taxonomy, consistency, completeness, incomplete classes, and
    // exclusions against the corresponding fixed-bound rooted fallback (switch OFF).
    let fixed_fb_limits = Limits {
        dl_work: Some(1),
        ..limits
    };
    let (out_fixed_fb, excl_fixed_fb) = with_rooted_adaptive_bound(false, || {
        with_rooted_adaptive_diag(false, || {
            let mut k = Classifier::of(&input);
            let out = k.classify(&fixed_fb_limits).expect("fixed fallback");
            (out, k.excluded().to_vec())
        })
    });
    assert!(out_fixed_fb.stats.dl_counting_over && out_fixed_fb.stats.dl_rooted_over);
    assert_eq!(out_fb.taxonomy, out_fixed_fb.taxonomy);
    assert_eq!(out_fb.consistency, out_fixed_fb.consistency);
    assert_eq!(out_fb.complete, out_fixed_fb.complete);
    assert_eq!(out_fb.incomplete, out_fixed_fb.incomplete);
    assert_eq!(excl_fb, excl_fixed_fb);
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
            let mut k = Classifier::of(&input);
            let (out, sample) = measure(|| k.classify(&limits).expect("fallback"));
            ((out, last_adaptive_attempts()), sample)
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

    // (b) Sol Blocker #1 / Sol r2 #3: pre-box `AdaptiveState` refusal at exact pre-box
    //     boundary (both without a pool and with a pool refusing the `pre_more` increment),
    //     explicitly witnessing that the attempt reached the adaptive pre-admission check,
    //     plus proving that saturation entry (`Point::Observed`) emits ZERO charge calls
    //     and that the first subsequent saturation charge occurs only after worker growth
    //     (`Point::BeforeDebit`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum MemEvent {
        Charge {
            bytes: usize,
            accepted: bool,
            paid_after: usize,
        },
        Observed,
        BeforeDebit,
    }
    let kinds: Vec<&AxiomKind> = input.iter().map(|a| &a.kind).collect();
    let baseline_limits = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        ..Limits::default()
    };
    let pre_box_base = with_rooted_adaptive_bound(false, || {
        with_rooted_adaptive_diag(false, || {
            let mut s = Saturation::new();
            let _ = s
                .run_once(&kinds, None, 0, &baseline_limits, true)
                .expect("baseline");
            LAST_ALLOWANCE.get().0
        })
    });
    assert!(pre_box_base > 0);
    let state_bytes = std::mem::size_of::<super::dl_rules::AdaptiveState>();

    // Calibrate exact `(norm_peak, pre_sat_exact, pre_more)` on a single rooted attempt.
    reset_adaptive_preadmit();
    LAST_ADAPTIVE_ATTEMPTS.with(|c| c.borrow_mut().clear());
    let (norm_peak, pre_sat_exact, pre_more) = with_rooted_adaptive_bound(true, || {
        let mut s = Saturation::new();
        let _ = s
            .run_once(&kinds, None, 0, &baseline_limits, true)
            .expect("calibrate adaptive preadmit");
        last_adaptive_preadmit().expect("must record pre-box adaptive admission")
    });
    assert_eq!(pre_sat_exact, pre_box_base + state_bytes);
    assert!(
        norm_peak < pre_sat_exact && pre_more == pre_sat_exact - norm_peak && pre_more > 0,
        "pre-box adaptive admission must require a positive increment above normalization peak: norm_peak={norm_peak}, pre_sat_exact={pre_sat_exact}, pre_more={pre_more}"
    );
    let refuse_cap = pre_box_base.max(norm_peak);
    assert!(refuse_cap < pre_sat_exact);

    // (b1) Without a pool: `max_memory == Some(refuse_cap)` passes normalization, reaches
    //      the adaptive pre-admission check, and refuses BEFORE allocating
    //      `Box<AdaptiveState>` and BEFORE starting saturation workers.
    let cap_at_base = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        max_memory: Some(refuse_cap),
        ..Limits::default()
    };
    reset_adaptive_alloc_count();
    reset_adaptive_preadmit();
    LAST_ADAPTIVE_ATTEMPTS.with(|c| c.borrow_mut().clear());
    let (res_base_cap, preadmit_b1, snaps_base_cap, allocs_base_cap) =
        with_rooted_adaptive_bound(true, || {
            let mut s = Saturation::new();
            let r = s.run_once(&kinds, None, 0, &cap_at_base, true);
            (
                r,
                last_adaptive_preadmit(),
                last_adaptive_attempts(),
                adaptive_alloc_count(),
            )
        });
    assert_eq!(
        preadmit_b1,
        Some((norm_peak, pre_sat_exact, pre_more)),
        "b1 must pass normalization and witness the pre-box adaptive admission check"
    );
    assert!(
        matches!(
            res_base_cap,
            Err(Error::Memory { limit, used }) if limit == refuse_cap && used == pre_sat_exact
        ),
        "b1 must return exact pre-box Error::Memory: {res_base_cap:?}"
    );
    assert_eq!(
        allocs_base_cap, 0,
        "max_memory == refuse_cap must refuse before allocating Box<AdaptiveState>"
    );
    assert!(snaps_base_cap.is_empty());

    // (b2) With a pool refusing any increment above `refuse_cap`: normalization succeeds
    //      up to `norm_peak`, the adaptive admission check requests exact `pre_more`, and
    //      the pool refuses it BEFORE `enable_adaptive` allocates.
    let events_b2: Arc<parking_lot::Mutex<Vec<MemEvent>>> = Arc::default();
    let paid_b2 = Arc::new(AtomicUsize::new(0));
    let events_b2_charge = events_b2.clone();
    let paid_b2_ref = paid_b2.clone();
    let pool_at_base = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        charge: Some(Arc::new(move |bytes: usize| {
            let cur = paid_b2_ref.load(Ordering::Relaxed);
            if cur.saturating_add(bytes) > refuse_cap {
                events_b2_charge.lock().push(MemEvent::Charge {
                    bytes,
                    accepted: false,
                    paid_after: cur,
                });
                return false;
            }
            let next = cur + bytes;
            paid_b2_ref.store(next, Ordering::Relaxed);
            events_b2_charge.lock().push(MemEvent::Charge {
                bytes,
                accepted: true,
                paid_after: next,
            });
            true
        })),
        ..Limits::default()
    };
    reset_adaptive_alloc_count();
    reset_adaptive_preadmit();
    LAST_ADAPTIVE_ATTEMPTS.with(|c| c.borrow_mut().clear());
    let (res_pool_refuse, preadmit_b2, snaps_pool_refuse, allocs_pool_refuse) =
        with_rooted_adaptive_bound(true, || {
            let mut s = Saturation::new();
            let r = s.run_once(&kinds, None, 0, &pool_at_base, true);
            (
                r,
                last_adaptive_preadmit(),
                last_adaptive_attempts(),
                adaptive_alloc_count(),
            )
        });
    assert_eq!(
        preadmit_b2,
        Some((norm_peak, pre_sat_exact, pre_more)),
        "b2 must pass normalization and witness the pre-box adaptive admission check"
    );
    assert!(
        matches!(
            res_pool_refuse,
            Err(Error::Memory { used, .. }) if used == pre_sat_exact
        ),
        "b2 must fail at pre_sat_exact: {res_pool_refuse:?}"
    );
    let log_b2 = events_b2.lock().clone();
    assert_eq!(
        log_b2.last().copied(),
        Some(MemEvent::Charge {
            bytes: pre_more,
            accepted: false,
            paid_after: norm_peak,
        }),
        "b2 must refuse the exact pre-box adaptive delta after normalization paid norm_peak: {log_b2:?}"
    );
    assert_eq!(
        allocs_pool_refuse, 0,
        "pool refusal at refuse_cap must happen before allocating Box<AdaptiveState>"
    );
    assert!(snaps_pool_refuse.is_empty());

    // (b3) Exact single-charge boundary check: a pool capped at EXACTLY `pre_sat_exact`
    //      (`pre_box_base + state_bytes`, zero extra headroom) must:
    //      1. grant `pre_more` up to `pre_sat_exact` BEFORE `Point::Observed`,
    //      2. allocate `Box<AdaptiveState>` once (`allocs == 1`),
    //      3. pass `saturate_counted`'s entry check (`Point::Observed`) with ZERO charge
    //         calls (proving `AdaptiveState` is NOT double-charged at entry),
    //      4. start workers (`Point::BeforeDebit`), and
    //      5. reject the first worker-growth debit (NOT an entry debit) cleanly with
    //         `WinningStop::Memory`.
    let events_b3: Arc<parking_lot::Mutex<Vec<MemEvent>>> = Arc::default();
    let paid_b3 = Arc::new(AtomicUsize::new(0));
    let events_b3_charge = events_b3.clone();
    let events_b3_hook = events_b3.clone();
    let paid_b3_ref = paid_b3.clone();
    let pool_exact_presat = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        charge: Some(Arc::new(move |bytes: usize| {
            let cur = paid_b3_ref.load(Ordering::Relaxed);
            if cur.saturating_add(bytes) > pre_sat_exact {
                events_b3_charge.lock().push(MemEvent::Charge {
                    bytes,
                    accepted: false,
                    paid_after: cur,
                });
                return false;
            }
            let next = cur + bytes;
            paid_b3_ref.store(next, Ordering::Relaxed);
            events_b3_charge.lock().push(MemEvent::Charge {
                bytes,
                accepted: true,
                paid_after: next,
            });
            true
        })),
        ..Limits::default()
    };
    reset_adaptive_alloc_count();
    reset_adaptive_preadmit();
    LAST_ADAPTIVE_ATTEMPTS.with(|c| c.borrow_mut().clear());
    let (res_exact, preadmit_b3, snaps_exact, allocs_exact) =
        with_rooted_adaptive_bound(true, || {
            admission_test::with_point_hook(
                move |point| match point {
                    admission_test::Point::Observed => {
                        events_b3_hook.lock().push(MemEvent::Observed);
                    }
                    admission_test::Point::BeforeDebit => {
                        events_b3_hook.lock().push(MemEvent::BeforeDebit);
                    }
                    _ => {}
                },
                || {
                    let mut s = Saturation::new();
                    let r = s.run_once(&kinds, None, 0, &pool_exact_presat, true);
                    (
                        r,
                        last_adaptive_preadmit(),
                        last_adaptive_attempts(),
                        adaptive_alloc_count(),
                    )
                },
            )
        });
    assert_eq!(preadmit_b3, Some((norm_peak, pre_sat_exact, pre_more)));
    let log_b3 = events_b3.lock().clone();
    let b3_first_obs = log_b3
        .iter()
        .position(|e| matches!(e, MemEvent::Observed))
        .expect("b3 must enter saturate_counted entry check");
    let b3_first_debit = log_b3
        .iter()
        .position(|e| matches!(e, MemEvent::BeforeDebit))
        .expect(
            "b3 must pass saturation entry without double-charging and reach worker BeforeDebit",
        );
    assert!(
        b3_first_debit > b3_first_obs,
        "b3 saturation entry must precede first worker debit"
    );
    assert_eq!(
        log_b3[..b3_first_obs]
            .iter()
            .rev()
            .find(|e| matches!(e, MemEvent::Charge { .. }))
            .copied(),
        Some(MemEvent::Charge {
            bytes: pre_more,
            accepted: true,
            paid_after: pre_sat_exact,
        }),
        "b3 must grant exact pre-box adaptive delta up to pre_sat_exact before saturation entry"
    );
    assert!(
        !log_b3[b3_first_obs..b3_first_debit]
            .iter()
            .any(|e| matches!(e, MemEvent::Charge { .. })),
        "b3 must NOT charge at saturation entry (would indicate double-charging AdaptiveState): {:?}",
        &log_b3[b3_first_obs..=b3_first_debit]
    );
    let b3_first_sat_charge = log_b3[b3_first_debit..]
        .iter()
        .find(|e| matches!(e, MemEvent::Charge { .. }))
        .copied();
    assert!(
        matches!(
            b3_first_sat_charge,
            Some(MemEvent::Charge {
                bytes,
                accepted: false,
                paid_after,
            }) if bytes > 0 && paid_after == pre_sat_exact
        ),
        "b3 first charge after entry must be a refused worker-growth debit after BeforeDebit: {b3_first_sat_charge:?}"
    );
    assert_eq!(
        allocs_exact, 1,
        "pool capped at exact pre_box_base + state_bytes must admit AdaptiveState once"
    );
    assert_eq!(snaps_exact.len(), 1);
    assert_eq!(snaps_exact[0].winning_stop, WinningStop::Memory);
    assert_eq!(snaps_exact[0].raw_queue, snaps_exact[0].oracle_queue);
    assert!(matches!(res_exact, Err(Error::Memory { .. })));

    // (b4) Wider-cap arm through full `Classifier::classify` with counting-allocator peak
    //      coverage: calibrate the `Classifier` attempt's pre-box admission, verify the
    //      pre-box grant, verify ZERO saturation-entry charges before `BeforeDebit`,
    //      identify the first accepted worker-growth debit, and refuse a later
    //      mid-saturation growth debit once to produce a partial `WinningStop::Memory`
    //      answer whose `peak_memory` covers `sample.peak`.
    let (clf_norm_peak, clf_pre_sat, clf_pre_more) = with_rooted_adaptive_bound(true, || {
        let mut k = Classifier::of(&input);
        let _ = k.classify(&baseline_limits).expect("calibrate classifier");
        last_adaptive_preadmit().expect("classifier must record pre-box adaptive admission")
    });
    assert!(
        clf_pre_sat > state_bytes
            && clf_norm_peak < clf_pre_sat
            && clf_pre_more == clf_pre_sat - clf_norm_peak
            && clf_pre_more > 0,
        "classifier pre-box adaptive admission must require a positive delta above normalization peak: clf_norm_peak={clf_norm_peak}, clf_pre_sat={clf_pre_sat}, clf_pre_more={clf_pre_more}"
    );
    // Pre-allocate the trace buffer so the test harness itself performs zero heap
    // allocations inside `measure(|| ...)` when recording `Charge`/`Observed`/`BeforeDebit`.
    let events_b4: Arc<parking_lot::Mutex<Vec<MemEvent>>> =
        Arc::new(parking_lot::Mutex::new(Vec::with_capacity(4096)));
    let paid_b4 = Arc::new(AtomicUsize::new(0));
    let rejected_b4 = Arc::new(AtomicBool::new(false));
    let events_b4_charge = events_b4.clone();
    let events_b4_hook = events_b4.clone();
    let paid_b4_ref = paid_b4.clone();
    let rejected_b4_ref = rejected_b4.clone();
    let push_capped = |buf: &parking_lot::Mutex<Vec<MemEvent>>, ev: MemEvent| {
        let mut g = buf.lock();
        if g.len() < g.capacity() {
            g.push(ev);
        }
    };
    let pool_tight_sat = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        charge: Some(Arc::new(move |bytes: usize| {
            let cur = paid_b4_ref.load(Ordering::Relaxed);
            if cur.saturating_add(bytes) > clf_pre_sat + (64 << 10)
                && !rejected_b4_ref.swap(true, Ordering::SeqCst)
            {
                push_capped(
                    &events_b4_charge,
                    MemEvent::Charge {
                        bytes,
                        accepted: false,
                        paid_after: cur,
                    },
                );
                return false;
            }
            let next = cur + bytes;
            paid_b4_ref.store(next, Ordering::Relaxed);
            push_capped(
                &events_b4_charge,
                MemEvent::Charge {
                    bytes,
                    accepted: true,
                    paid_after: next,
                },
            );
            true
        })),
        ..Limits::default()
    };
    reset_adaptive_alloc_count();
    let ((res_tight, preadmit_b4, snaps_tight, allocs_tight), sample_tight) =
        with_rooted_adaptive_bound(true, || {
            admission_test::with_point_hook(
                move |point| match point {
                    admission_test::Point::Observed => {
                        push_capped(&events_b4_hook, MemEvent::Observed);
                    }
                    admission_test::Point::BeforeDebit => {
                        push_capped(&events_b4_hook, MemEvent::BeforeDebit);
                    }
                    _ => {}
                },
                || {
                    let mut k = Classifier::of(&input);
                    let (r, sample) = measure(|| k.classify(&pool_tight_sat));
                    (
                        (
                            r,
                            last_adaptive_preadmit(),
                            last_adaptive_attempts(),
                            adaptive_alloc_count(),
                        ),
                        sample,
                    )
                },
            )
        });
    assert_eq!(
        preadmit_b4,
        Some((clf_norm_peak, clf_pre_sat, clf_pre_more))
    );
    let log_b4 = events_b4.lock().clone();
    let b4_first_obs = log_b4
        .iter()
        .position(|e| matches!(e, MemEvent::Observed))
        .expect("b4 must hit saturation entry Observed");
    let b4_first_debit = log_b4
        .iter()
        .position(|e| matches!(e, MemEvent::BeforeDebit))
        .expect("b4 must start workers and hit BeforeDebit");
    assert!(
        b4_first_debit > b4_first_obs,
        "b4 saturation entry must precede first worker debit"
    );
    assert_eq!(
        log_b4[..b4_first_obs]
            .iter()
            .rev()
            .find(|e| matches!(e, MemEvent::Charge { .. }))
            .copied(),
        Some(MemEvent::Charge {
            bytes: clf_pre_more,
            accepted: true,
            paid_after: clf_pre_sat,
        }),
        "b4 must grant exact pre-box adaptive delta up to clf_pre_sat before saturation entry"
    );
    assert!(
        !log_b4[b4_first_obs..b4_first_debit]
            .iter()
            .any(|e| matches!(e, MemEvent::Charge { .. })),
        "b4 must NOT charge again at saturation entry before first worker debit: {:?}",
        &log_b4[b4_first_obs..=b4_first_debit]
    );
    let b4_first_sat_charge = log_b4[b4_first_debit..]
        .iter()
        .find(|e| matches!(e, MemEvent::Charge { .. }))
        .copied();
    assert!(
        matches!(
            b4_first_sat_charge,
            Some(MemEvent::Charge {
                bytes,
                accepted: true,
                paid_after,
            }) if bytes > 0 && paid_after == clf_pre_sat + bytes
        ),
        "b4 first saturation charge must occur after worker BeforeDebit and grow above clf_pre_sat ({clf_pre_sat}): {b4_first_sat_charge:?}"
    );
    assert_eq!(allocs_tight, 1, "AdaptiveState must be allocated once");
    assert!(
        rejected_b4.load(Ordering::SeqCst),
        "tight saturation pool must cleanly reject a mid-saturation debit"
    );
    assert_eq!(snaps_tight.len(), 1);
    assert_eq!(snaps_tight[0].winning_stop, WinningStop::Memory);
    assert_eq!(snaps_tight[0].raw_queue, snaps_tight[0].oracle_queue);
    let out_tight = res_tight.expect("partial memory stop");
    assert!(!out_tight.complete && !out_tight.complete_for_taken);
    assert_eq!(out_tight.consistency, Consistency::Unknown);
    assert!(!out_tight.stats.dl_rooted_over && !out_tight.stats.dl_over_budget);
    assert!(
        out_tight.stats.peak_memory >= sample_tight.peak,
        "tight cap peak_memory ({}) undercounted measured peak heap ({})",
        out_tight.stats.peak_memory,
        sample_tight.peak
    );
}

/// 11. Sol Blocker #2 & Neon Note #3: `ADAPTIVE-OUTCOME` reports the EXACT final public
///     classification outcome (after `Classification::new`, exclusion `cover`, and `strict`),
///     and stale snapshots/outcomes are cleared at top-level entry before `interchangeable()`
///     can return early on cancellation or timeout.
#[test]
fn adaptive_outcome_matches_public_result_and_clears_on_early_error() {
    let mut body = String::from(
        "SubClassOf(:C ObjectMaxCardinality(12 :r owl:Thing))\n\
         SubClassOf(:Seed ObjectUnionOf(:U0 :U1))\n\
         SubClassOf(:U0 :Done)\n\
         SubClassOf(:U1 :Done)\n",
    );
    for k in 0..13 {
        body.push_str(&format!("SubClassOf(:C ObjectSomeValuesFrom(:r :B{k}))\n"));
    }
    for i in 0..20 {
        body.push_str(&format!("ClassAssertion(:Seed :i{i})\n"));
    }
    let counting_input = axioms(&body);

    // (a) `strict = true` after a counting exclusion returns `Err(Error::Incomplete)`:
    //     `ADAPTIVE-OUTCOME` must report `Err { kind: "incomplete" }`, NOT `Ok`!
    let strict_limits = Limits {
        threads: 1,
        counting: true,
        rooted: true,
        dl_steps: true,
        strict: true,
        ..Limits::default()
    };
    let (res_strict, outcome_strict, snaps_strict) = with_rooted_adaptive_bound(true, || {
        with_rooted_adaptive_diag(true, || {
            let mut k = Classifier::of(&counting_input);
            let r = k.classify(&strict_limits);
            (r, last_adaptive_outcome(), last_adaptive_attempts())
        })
    });
    assert!(matches!(res_strict, Err(Error::Incomplete(_))));
    assert_eq!(snaps_strict.len(), 1);
    assert!(matches!(
        outcome_strict,
        Some(AdaptiveOutcome::Err {
            run_id,
            attempts: 1,
            kind: "incomplete",
        }) if run_id > 0
    ));

    // (b) `strict = false` on the same ontology runs `out.cover(...)`: `ADAPTIVE-OUTCOME`
    //     must report the exact post-coverage `complete`, `consistency`, `incomplete`, and
    //     `excluded` returned to the caller!
    let cover_limits = Limits {
        strict: false,
        ..strict_limits
    };
    let (out_cov, excl_cov, outcome_cov) = with_rooted_adaptive_bound(true, || {
        with_rooted_adaptive_diag(true, || {
            let mut k = Classifier::of(&counting_input);
            let out = k.classify(&cover_limits).expect("covered answer");
            (out, k.excluded().to_vec(), last_adaptive_outcome())
        })
    });
    assert!(!out_cov.complete);
    assert!(!excl_cov.is_empty());
    assert!(matches!(
        outcome_cov,
        Some(AdaptiveOutcome::Ok {
            run_id,
            attempts: 1,
            consistency,
            complete,
            complete_for_taken,
            dl_counting_over: true,
            dl_rooted_over: false,
            excluded,
            incomplete,
            ..
        }) if run_id > 0
            && consistency == out_cov.consistency
            && complete == out_cov.complete
            && complete_for_taken == out_cov.complete_for_taken
            && excluded == excl_cov.len()
            && incomplete == out_cov.incomplete.len()
    ));

    // (c) `partial = true` + `Stop::Memory`: `ADAPTIVE-OUTCOME` must report
    //     `consistency == Consistency::Unknown`, `complete == false`, `incomplete > 0`.
    let partial_input = rooted_disjunctive_ontology(60, 15);
    let small_root_policy = AdaptivePolicy {
        sample_shift: 4,
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 16,
        warmup_work: 1 << 20,
        queue_max: 1 << 20,
        ceiling: 1 << 20,
    };
    let latched = Arc::new(AtomicBool::new(false));
    let refused = Arc::new(AtomicBool::new(false));
    let latched_h = latched.clone();
    let latched_c = latched.clone();
    let refused_c = refused.clone();
    let partial_limits = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        partial: true,
        charge: Some(Arc::new(move |_| {
            if latched_c.load(Ordering::SeqCst) && !refused_c.swap(true, Ordering::SeqCst) {
                return false;
            }
            true
        })),
        ..Limits::default()
    };
    let (out_part, outcome_part) = with_rooted_adaptive_policy(small_root_policy, || {
        with_rooted_adaptive_bound(true, || {
            with_rooted_adaptive_diag(true, || {
                with_on_adaptive_latch(
                    move |g, _| {
                        if g == WorkStopGuard::RootWork {
                            latched_h.store(true, Ordering::SeqCst);
                        }
                    },
                    || {
                        let mut k = Classifier::of(&partial_input);
                        let out = k.classify(&partial_limits).expect("partial memory");
                        (out, last_adaptive_outcome())
                    },
                )
            })
        })
    });
    assert_eq!(out_part.consistency, Consistency::Unknown);
    assert!(!out_part.complete && !out_part.incomplete.is_empty());
    assert!(matches!(
        outcome_part,
        Some(AdaptiveOutcome::Ok {
            consistency: Consistency::Unknown,
            complete: false,
            complete_for_taken: false,
            incomplete,
            ..
        }) if incomplete == out_part.incomplete.len()
    ));

    // (d) Early cancellation inside `interchangeable()` must clear stale snapshots and
    //     outcome from the preceding classification instead of leaving them visible.
    let mut k = Classifier::of(&partial_input);
    let normal_limits = Limits {
        threads: 1,
        rooted: true,
        dl_steps: true,
        ..Limits::default()
    };
    with_rooted_adaptive_bound(true, || {
        with_rooted_adaptive_diag(true, || {
            let _ = k.classify(&normal_limits).expect("seed snapshots");
            assert_eq!(last_adaptive_attempts().len(), 1);
            assert!(last_adaptive_outcome().is_some());
        })
    });
    let cancelled_immediately = Limits {
        cancelled: Some(Arc::new(|| true)),
        ..normal_limits
    };
    let early_err = k.classify(&cancelled_immediately);
    assert!(matches!(early_err, Err(Error::Cancelled)));
    assert!(
        last_adaptive_attempts().is_empty(),
        "early interchangeable() cancellation must clear prior adaptive snapshots"
    );
    assert!(
        last_adaptive_outcome().is_none(),
        "early interchangeable() cancellation must clear prior adaptive outcome"
    );
}

/// 12. Sol r2 #1 regression: marking-round disarm is strictly scoped to adaptive
///     enforcement; non-adaptive and observe-only runs preserve verbatim fixed-limit,
///     width-limit, and forced-exhaustion (`Found::over`) enforcement while stopping
///     post-saturation adaptive sampling independently.
#[test]
fn marking_disarm_preserves_fixed_and_observe_only_enforcement() {
    let policy = AdaptivePolicy {
        sample_shift: 4, // sample every 16 work
        ratio_num: 3,
        ratio_den: 10,
        root_limit: 8,
        warmup_work: 1 << 20,
        queue_max: 1 << 20,
        ceiling: 64,
    };

    // (a) Non-adaptive `DlRun` (switch OFF / explicit fixed / counted / unrooted):
    //     `disarm_adaptive_work_bound()` must leave `work_limit`, `width_limit`, and
    //     `exhausted` untouched.
    let fixed_run = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    fixed_run.set_work_limit(10);
    fixed_run.set_width_limit(2);
    fixed_run.count(11);
    assert!(fixed_run.over_work());
    fixed_run.disarm_adaptive_work_bound();
    assert!(fixed_run.bounded(), "fixed work_limit must remain intact");
    assert!(
        fixed_run.over_work(),
        "fixed run with work > work_limit must still report over_work after disarm"
    );
    let wide = vec![vec![(vec![], vec![]); 3]];
    assert!(
        fixed_run.too_wide(&wide),
        "fixed width_limit must remain intact"
    );

    let fixed_exhaust = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    fixed_exhaust.set_work_limit(100);
    fixed_exhaust.exhaust();
    fixed_exhaust.disarm_adaptive_work_bound();
    assert!(
        fixed_exhaust.over_work(),
        "fixed run with exhausted=true must still report over_work after disarm"
    );

    // (b) Observe-only mode (`enforce = false`, `diag = true`):
    //     `stop_adaptive_sampling()` + `disarm_adaptive_work_bound()` must stop further
    //     adaptive sampling (`samples` and `hypothetical_guard` frozen) while still
    //     counting work, enforcing fixed `work_limit`, and letting a fresh `exhaust()`
    //     (`Found::over`) set `exhausted` and trigger `over_work()`.
    let mut obs_run = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    obs_run.set_work_limit(20);
    obs_run.enable_adaptive(policy, false, false, make_meta("default_fixed", 20));
    obs_run.count(16);
    assert_eq!(obs_run.adaptive_samples(), 1);
    assert!(!obs_run.over_work());
    obs_run.stop_adaptive_sampling();
    obs_run.disarm_adaptive_work_bound();
    // Cross another sample boundary and exceed `root_limit` + `work_limit`: sampling
    // stays stopped, but fixed `work_limit` enforcement MUST still fire!
    obs_run.count_root(100);
    obs_run.count(16);
    assert_eq!(
        obs_run.adaptive_samples(),
        1,
        "observe-only sampling must stay stopped after joined boundary"
    );
    assert_eq!(
        obs_run.hypothetical_guard(),
        WorkStopGuard::None,
        "observe-only must not latch post-saturation hypothetical guards"
    );
    assert_eq!(obs_run.work(), 32);
    assert!(
        obs_run.over_work(),
        "observe-only must preserve fixed work_limit enforcement during marking"
    );

    // Fresh `Found::over` (`exhaust()`) in observe-only mode after sampling stopped:
    let mut obs_exhaust = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    obs_exhaust.set_work_limit(10_000);
    obs_exhaust.enable_adaptive(policy, false, false, make_meta("default_fixed", 10_000));
    obs_exhaust.stop_adaptive_sampling();
    obs_exhaust.disarm_adaptive_work_bound();
    assert!(!obs_exhaust.over_work());
    obs_exhaust.exhaust();
    assert!(
        obs_exhaust.over_work(),
        "observe-only must still honor fresh Found::over during marking, matching diag-off"
    );

    // (c) Enforced adaptive mode (`enforce = true`):
    //     `disarm_adaptive_work_bound()` suppresses late `Stop::Work` on a latched guard,
    //     ceiling excess, or `exhaust()`, while keeping `AdaptiveState` in `bytes()`.
    let mut enf_run = DlRun::new(false, Arc::new(AtomicBool::new(false)));
    enf_run.enable_adaptive(policy, true, false, make_meta("rawq_v1", policy.ceiling));
    let bytes_before = enf_run.bytes();
    assert!(bytes_before >= std::mem::size_of::<super::dl_rules::AdaptiveState>());
    enf_run.count_root(10);
    enf_run.count(16);
    assert_eq!(enf_run.stop_guard(), WorkStopGuard::RootWork);
    assert!(enf_run.over_work());
    enf_run.stop_adaptive_sampling();
    enf_run.disarm_adaptive_work_bound();
    assert!(
        !enf_run.over_work(),
        "disarmed enforced adaptive run must not re-fire stale stop_guard in marking"
    );
    enf_run.count(policy.ceiling + 16);
    assert!(!enf_run.past(policy.ceiling + 16));
    enf_run.exhaust();
    assert!(!enf_run.over_work());
    assert_eq!(
        enf_run.adaptive_samples(),
        1,
        "enforced sampling must stay stopped"
    );
    assert_eq!(
        enf_run.bytes(),
        bytes_before,
        "AdaptiveState must remain accounted in DlRun::bytes()"
    );
}
