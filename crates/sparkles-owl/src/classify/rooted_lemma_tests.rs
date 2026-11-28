//! Falsification tests for the lemmas of item 1220's proof of the repaired ground-body
//! selection and of item 1250's ground-body cut (F24, "Item 804's departures from the
//! journal's rules" and the Outcome). Each test would fail on a counterexample to the
//! statement it checks. The census tests of L1 and L2 also check that their family of
//! ontologies reaches the clauses that the statements are about, in aggregate and not per
//! file. The fifth obligation's argument and the sixth's check of the return's filter are
//! code reads with no test here.
//!
//! - L1, the flags. A clause of a context other than the root that reached rule processing
//!   in a finished saturation has `gbody` exactly when the cut or the selection is active
//!   and its body holds a literal with neither x nor y, and `sel` exactly when the
//!   selection is active and it does. No root clause has either, and so no clause with an
//!   empty body is cut, which is the stage of facts of item 1250's argument. A saturation
//!   that stops after a clause is marked processed and before the rules run on it can leave
//!   that clause with its initial flags, so the census reads finished saturations only.
//! - L2, the bodies, which closes item 1250's fourth obligation. No body of a context other
//!   than the root holds a literal over a successor term `f_e(u)` of a constant, and every
//!   body literal of kind XL there is over constants, which Join takes as its pivot.
//! - The arguments that the proof leaves relative or open, item 1250's discharge in the
//!   query context, its first three obligations, the repaired selection and item 1220's
//!   Eq/Factor query case, are tested as regressions on ontologies built around each of
//!   them against HermiT 1.4.5, in every arm of the switches. The comparison covers the
//!   unsatisfiable named classes and the subsumptions between declared classes, and ignores
//!   the records of `owl:Thing`. Controls show that the discharge loses without the Join
//!   exemption and the selection loses without its repair. The query fixtures `hq1` and
//!   `hq2` are taxonomy regressions only, since no test shows that their runs reach a
//!   query-shaped Eq or Factor step.
//!
//! The arms are set per thread through [`super::dl_rules::set_test_arm`], since the
//! switches read the environment once per process otherwise. The census checks that the
//! flags follow the arm, so a saturation that ran on another thread fails L1 instead of
//! passing in the wrong arm.

use super::dl_rules::{LemmaCensus, TestArm, set_test_arm};
use super::*;
use std::cell::RefCell;
use std::collections::BTreeSet;

struct Observed {
    /// the arm that the test requested, which the census's expected flags read instead of
    /// the switches that the rules read
    arm: TestArm,
    runs: Vec<Vec<LemmaCensus>>,
    unfinished: usize,
}

thread_local! {
    static CENSUS: RefCell<Option<Observed>> = const { RefCell::new(None) };
}

/// Whether a test on this thread observes the census.
pub(super) fn observing() -> bool {
    CENSUS.with(|a| a.borrow().is_some())
}

/// The arm that the observing test on this thread requested, if any.
pub(super) fn requested_arm() -> Option<TestArm> {
    CENSUS.with(|a| a.borrow().as_ref().map(|o| o.arm))
}

/// The census of one context of the saturation that is ending.
pub(super) fn record(c: LemmaCensus) {
    CENSUS.with(|a| {
        if let Some(o) = a.borrow_mut().as_mut() {
            if o.runs.is_empty() {
                o.runs.push(Vec::new());
            }
            o.runs.last_mut().expect("a saturation").push(c);
        }
    });
}

/// The end of a saturation that the census read.
pub(super) fn saturated() {
    CENSUS.with(|a| {
        if let Some(o) = a.borrow_mut().as_mut() {
            o.runs.push(Vec::new());
        }
    });
}

/// The end of a saturation that stopped before it finished, which the census skips.
pub(super) fn unfinished() {
    CENSUS.with(|a| {
        if let Some(o) = a.borrow_mut().as_mut() {
            o.unfinished += 1;
        }
    });
}

/// Clears the observation and the arm however the test leaves the thread.
struct Observe;

impl Observe {
    fn start(arm: TestArm) -> Observe {
        set_test_arm(Some(arm));
        CENSUS.with(|a| {
            *a.borrow_mut() = Some(Observed {
                arm,
                runs: Vec::new(),
                unfinished: 0,
            })
        });
        Observe
    }
    fn take(self) -> Observed {
        CENSUS.with(|a| a.borrow_mut().take()).expect("observed")
    }
}

impl Drop for Observe {
    fn drop(&mut self) {
        CENSUS.with(|a| *a.borrow_mut() = None);
        set_test_arm(None);
    }
}

/// The default arm, the cut of item 1250 with the selection off, and the uncut control.
const ARMS: [(&str, TestArm); 3] = [
    (
        "default",
        TestArm {
            select_ground: true,
            ground_body_cut: true,
            sel_partners_x: true,
            join_exempt: 2,
        },
    ),
    (
        "cut",
        TestArm {
            select_ground: false,
            ground_body_cut: true,
            sel_partners_x: false,
            join_exempt: 2,
        },
    ),
    (
        "uncut",
        TestArm {
            select_ground: false,
            ground_body_cut: false,
            sel_partners_x: false,
            join_exempt: 2,
        },
    ),
];

/// The census totals of one classification.
#[derive(Default, Debug)]
struct Totals {
    saturations: usize,
    unfinished: usize,
    processed: usize,
    gbody: usize,
    /// ground-bodied clauses with a head over `f_e(u)` in contexts that are neither the
    /// root nor a query context, the holders of item 1250's second obligation
    fc_head_gbody_outside: usize,
    /// ground-bodied clauses in query contexts, a broad count that does not check whether
    /// the context is reused as a successor
    gbody_in_query: usize,
    /// selected clauses of query contexts with a maximal literal `x ≈ u`, a count of shapes
    /// that witnesses no Eq or Factor step
    sel_eq_x_in_query: usize,
    xy_body: usize,
    xy_bodies: Vec<String>,
    fc_body: Vec<String>,
    flags: Vec<String>,
}

struct Run {
    totals: Totals,
    complete: bool,
    out: Classification,
}

/// Classifies with the rooted rules in `arm` on one thread, so that the arm, which is set
/// per thread, reaches the saturation. `work` is the rooted bound, `None` for the default.
fn classify(
    ax: &[crate::model::Axiom],
    arm: TestArm,
    eager: bool,
    counting: bool,
    work: Option<usize>,
) -> Run {
    let mut k = Classifier::of(ax);
    let observe = Observe::start(arm);
    let out = k.classify(&Limits {
        threads: 1,
        counting,
        rooted: true,
        dl_work: work,
        expansion: if eager {
            Expansion::Eager
        } else {
            Expansion::Cautious
        },
        ..Limits::default()
    });
    let seen = observe.take();
    let out = out.expect("a classification");
    let mut t = Totals {
        unfinished: seen.unfinished,
        ..Totals::default()
    };
    for r in seen.runs.into_iter().filter(|r| !r.is_empty()) {
        if !r.iter().all(|c| c.rooted) {
            continue;
        }
        t.saturations += 1;
        for c in r {
            t.processed += c.processed;
            t.gbody += c.gbody;
            if !c.root && !c.query {
                t.fc_head_gbody_outside += c.fc_head_gbody;
            }
            if c.query {
                t.gbody_in_query += c.gbody;
                t.sel_eq_x_in_query += c.sel_eq_x;
            }
            t.xy_body += c.xy_body;
            t.xy_bodies.extend(c.xy_bodies);
            t.fc_body.extend(c.fc_body);
            t.flags.extend(c.flags);
        }
    }
    let st = &out.stats;
    let complete = out.complete
        && out.incomplete.is_empty()
        && k.excluded().is_empty()
        && !st.dl_over_budget
        && !st.dl_counting_over
        && !st.dl_rooted_over
        && !st.dl_rooted_declined;
    Run {
        totals: t,
        complete,
        out,
    }
}

fn fixture_dir(sub: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(sub)
}

/// The ontologies of the census: the `shoi` fixtures, the hand cases with nominals, keys
/// and the universal property, item 1061's fixtures and this item's own.
fn census_files() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for (dir, keep) in [
        (
            "dl603",
            &(|n: &str| {
                (n.starts_with("shoi-")
                    || n.starts_with("hand-nominal")
                    || n.starts_with("hand-key")
                    || n.starts_with("hand-top"))
                    && !n.contains(".hermit")
                    && !n.contains(".konclude")
            }) as &dyn Fn(&str) -> bool,
        ),
        ("dl1061", &|_: &str| true),
        ("dl1220", &|_: &str| true),
    ] {
        let mut v: Vec<_> = std::fs::read_dir(fixture_dir(dir))
            .expect("fixture directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|x| x == "ofn")
                    && p.file_name().and_then(|n| n.to_str()).is_some_and(keep)
            })
            .collect();
        v.sort();
        out.extend(v);
    }
    out
}

/// The axioms of a fixture, which must read and parse.
fn axioms_of(path: &std::path::Path) -> Vec<crate::model::Axiom> {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    crate::ofn_read::parse(&text)
        .unwrap_or_else(|e| panic!("parsing {}: {e:?}", path.display()))
        .axioms
}

/// L1 and L2 over every census ontology in every arm, under both strategies and with counting
/// under the cautious one. Each arm must
/// meet ground-bodied clauses where its flags allow them and none where they do not, and
/// the cut arms must meet ground-bodied clauses with a head over `f_e(u)` outside the
/// query contexts, the holders of item 1250's second obligation, so that the census is not
/// vacuous.
#[test]
fn census_flags_follow_bodies_and_no_body_holds_a_successor_term_of_a_constant() {
    let files = census_files();
    assert!(files.len() >= 60, "census ontologies: {}", files.len());
    for (name, arm) in ARMS {
        let mut sum = Totals::default();
        for path in &files {
            let ax = axioms_of(path);
            for (eager, counting) in [(false, false), (true, false), (false, true)] {
                let r = classify(&ax, arm, eager, counting, Some(200_000));
                let t = r.totals;
                if let Some(f) = t.flags.first() {
                    panic!("L1 in arm {name} on {}: {f}", path.display());
                }
                if let Some(f) = t.fc_body.first() {
                    panic!("L2 in arm {name} on {}: {f}", path.display());
                }
                if let Some(f) = t.xy_bodies.first() {
                    panic!("L2 (x or y) in arm {name} on {}: {f}", path.display());
                }
                sum.saturations += t.saturations;
                sum.unfinished += t.unfinished;
                sum.processed += t.processed;
                sum.gbody += t.gbody;
                sum.fc_head_gbody_outside += t.fc_head_gbody_outside;
                sum.gbody_in_query += t.gbody_in_query;
                sum.sel_eq_x_in_query += t.sel_eq_x_in_query;
                sum.xy_body += t.xy_body;
            }
        }
        eprintln!("census arm {name}: {sum:?}");
        assert!(
            sum.saturations >= 100,
            "arm {name} read too few saturations: {sum:?}"
        );
        assert!(
            sum.processed > 10_000,
            "arm {name} read too few clauses: {sum:?}"
        );
        if arm.select_ground || arm.ground_body_cut {
            assert!(
                sum.gbody > 0,
                "arm {name} met no ground-bodied clause: {sum:?}"
            );
            assert!(
                sum.fc_head_gbody_outside > 0,
                "arm {name} met no ground-bodied clause over f_e(u) outside a query context: {sum:?}"
            );
            assert!(
                sum.gbody_in_query > 0,
                "arm {name} met no ground-bodied query clause: {sum:?}"
            );
        } else {
            assert_eq!(sum.gbody, 0, "the uncut arm cut a clause: {sum:?}");
        }
    }
}

/// The answer of HermiT 1.4.5 that a fixture's `.hermit` file records, in the format of
/// the lane's `BatchHermit`: `UNSAT iri`, `SUB a b` for every entailed pair of distinct
/// named classes, `TOP iri`, and `END`.
fn hermit(path: &std::path::Path) -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let text = std::fs::read_to_string(path.with_extension("hermit")).expect("hermit answer");
    assert!(
        text.trim_end().ends_with("END"),
        "unfinished answer {}",
        path.display()
    );
    let mut unsat = BTreeSet::new();
    let mut subs = BTreeSet::new();
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["UNSAT", c] if !c.ends_with("#Nothing") && !c.ends_with("#Thing") => {
                unsat.insert(c.to_string());
            }
            ["SUB", a, b] => {
                subs.insert((a.to_string(), b.to_string()));
            }
            _ => {}
        }
    }
    (unsat, subs)
}

/// The run's answer in the same form, over the named classes that the ontology declares.
fn ours(
    ax: &[crate::model::Axiom],
    out: &Classification,
) -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    use crate::model::{AxiomKind, Entity};
    let classes: Vec<Class> = ax
        .iter()
        .filter_map(|a| match &a.kind {
            AxiomKind::Declaration(Entity::Class(c)) => Some(c.clone()),
            _ => None,
        })
        .collect();
    let mut unsat = BTreeSet::new();
    let mut subs = BTreeSet::new();
    for a in &classes {
        if out.taxonomy.is_unsatisfiable(a) {
            unsat.insert(a.as_str().to_string());
            continue;
        }
        for b in &classes {
            if a != b && out.taxonomy.is_subclass(a, b) {
                subs.insert((a.as_str().to_string(), b.as_str().to_string()));
            }
        }
    }
    (unsat, subs)
}

/// The ontologies built around the relative and open parts of the proof, each with the
/// part it attacks, whether the cut arms must meet a ground-bodied clause over `f_e(u)`
/// outside a query context on it, and whether it is a query fixture, for which the test
/// reports the selected `x ≈ u` maxima of query contexts without asserting them. Such a
/// count witnesses neither reuse of the context as a successor nor an Eq target or a
/// Factor pair, so the query fixtures are taxonomy regressions.
const TARGETED: &[(&str, &str, bool, bool)] = &[
    // K mentions only v and its successor D mentions u, with f(u) made in D under a residue;
    // the successor of f(u) applies w's told facts at x, so no holder of obligation 2 forms
    (
        "hc1",
        "a split holder with f(u) in D under a residue",
        false,
        false,
    ),
    // as hc1 with the fact at w derived in the root; the residue travels at y, so K reasons at u
    (
        "hc2",
        "a split holder with the fact at w derived in the root",
        false,
        false,
    ),
    (
        "hc3",
        "obligations 1 and 2, f(u) made in the root and f(u) = w under a marker",
        true,
        false,
    ),
    (
        "hd1",
        "the discharge of two ground body literals in K, the first producer delayed",
        false,
        false,
    ),
    (
        "hd3",
        "the discharge of two ground body literals in K with an empty head",
        false,
        false,
    ),
    (
        "hd2",
        "the discharge of two ground body literals in K, the second producer delayed",
        false,
        false,
    ),
    (
        "hq1",
        "item 1220's Eq query case, a named class reused as a successor with x = u under a marker",
        false,
        true,
    ),
    (
        "hq2",
        "item 1220's Factor query case, x = u or x = w under a marker",
        false,
        true,
    ),
    // the three files of the generated set g1 (gen1220.py) with the most holders of
    // obligation 2 that every arm answers in full
    ("g00080", "obligation 2's holders, generated", true, false),
    ("g00126", "obligation 2's holders, generated", true, false),
    (
        "g01430",
        "obligation 2's holders and query clauses, generated",
        true,
        false,
    ),
];

/// Every targeted ontology gives HermiT's unsatisfiable classes and subsumptions between
/// declared classes in full in the default and cut arms under both strategies, and the cut
/// arms meet the holders of the second obligation where the table says so. The uncut arm is
/// a control that may pass the rooted bound, and it must agree when it answers in full.
#[test]
fn targeted_ontologies_agree_with_hermit_in_every_arm() {
    for &(name, what, fc_outside, query_eq) in TARGETED {
        let path = fixture_dir("dl1220").join(format!("{name}.ofn"));
        let ax = axioms_of(&path);
        let want = hermit(&path);
        for (arm_name, arm) in ARMS {
            for eager in [false, true] {
                let r = classify(&ax, arm, eager, false, None);
                let at = format!("{name} ({what}) in arm {arm_name}, eager {eager}");
                let cuts = arm.select_ground || arm.ground_body_cut;
                if !cuts && !r.complete {
                    continue;
                }
                assert!(
                    r.complete,
                    "{at}: not a complete answer, stats {:?}",
                    r.out.stats
                );
                assert!(
                    r.totals.saturations > 0,
                    "{at}: no rooted saturation finished"
                );
                assert!(r.totals.flags.is_empty(), "{at}: L1 {:?}", r.totals.flags);
                assert!(
                    r.totals.fc_body.is_empty() && r.totals.xy_bodies.is_empty(),
                    "{at}: L2 {:?} {:?}",
                    r.totals.fc_body,
                    r.totals.xy_bodies
                );
                assert_eq!(ours(&ax, &r.out), want, "{at}: differs from HermiT");
                if arm.select_ground || arm.ground_body_cut {
                    if fc_outside {
                        assert!(
                            r.totals.fc_head_gbody_outside > 0,
                            "{at}: met no ground-bodied clause over f_e(u) outside a query context"
                        );
                    }
                    // a selected `x ≈ u` maximum in a query context witnesses no Eq or
                    // Factor step, so the count is reported and never asserted
                    if query_eq && arm.select_ground {
                        eprintln!(
                            "{at}: selected x = u maxima in query contexts: {}",
                            r.totals.sel_eq_x_in_query
                        );
                    }
                }
            }
        }
    }
}

/// The control for item 1220's repaired selection R1. With the selection on and without
/// R1, the run that the repair replaced, `hd1` and `hd2` answer in full and miss
/// `A ⊑ Goal`, which every arm of [`ARMS`] finds, since two selected clauses of D's context
/// never meet in Hyper over x. So the comparison with HermiT and the arm set for the thread
/// can see the loss that R1 repairs.
#[test]
fn without_the_repair_the_selection_loses_the_answer_of_hd1_and_hd2() {
    let orig = TestArm {
        select_ground: true,
        ground_body_cut: true,
        sel_partners_x: false,
        join_exempt: 2,
    };
    for name in ["hd1", "hd2"] {
        let path = fixture_dir("dl1220").join(format!("{name}.ofn"));
        let ax = axioms_of(&path);
        let want = hermit(&path);
        let goal = (
            "http://example.org/".to_string() + name + "#A",
            "http://example.org/".to_string() + name + "#Goal",
        );
        assert!(want.1.contains(&goal), "{name}: HermiT gives A ⊑ Goal");
        for eager in [false, true] {
            let r = classify(&ax, orig, eager, false, None);
            assert!(r.complete, "{name}, eager {eager}: a complete answer");
            let got = ours(&ax, &r.out);
            assert!(
                !got.1.contains(&goal),
                "{name}, eager {eager}: the run without R1 finds A ⊑ Goal, so the control no longer loses"
            );
        }
    }
}

/// The control for L3. In `hd3` the query context holds a clause with two ground body
/// literals and an empty head, from a successor whose class is unsatisfiable, and one of the
/// two producers is delayed. L3's induction needs the Join exemption, since the first Join
/// leaves a clause with a ground body and neither x nor y. Without the exemption the run
/// answers in full and finds `A` satisfiable, with the selection on and with the cut alone,
/// while every arm of [`ARMS`] finds it unsatisfiable.
#[test]
fn without_the_join_exemption_the_discharge_of_hd3_is_lost() {
    let path = fixture_dir("dl1220").join("hd3.ofn");
    let ax = axioms_of(&path);
    let a = "http://example.org/hd3#A".to_string();
    assert!(hermit(&path).0.contains(&a), "HermiT finds A unsatisfiable");
    for (name, arm) in &ARMS[..2] {
        let arm = TestArm {
            join_exempt: 0,
            ..*arm
        };
        for eager in [false, true] {
            let r = classify(&ax, arm, eager, false, None);
            assert!(
                r.complete,
                "arm {name} without the exemption, eager {eager}"
            );
            assert!(
                !ours(&ax, &r.out).0.contains(&a),
                "arm {name} without the exemption, eager {eager}: A is unsatisfiable, so the control no longer loses"
            );
        }
    }
}

/// L1 on the flags of the stored clauses directly: the same clause gets the flags of each
/// arm, a marker over x does not make a clause ground-bodied, and an empty body never does.
#[test]
fn flags_of_each_arm_follow_the_body() {
    let path = fixture_dir("dl1220").join("hd1.ofn");
    let ax = axioms_of(&path);
    for (name, arm) in ARMS {
        let r = classify(&ax, arm, false, false, Some(200_000));
        assert!(r.complete, "arm {name}");
        assert!(
            r.totals.flags.is_empty(),
            "arm {name}: {:?}",
            r.totals.flags
        );
        if arm.select_ground || arm.ground_body_cut {
            assert!(r.totals.gbody > 0, "arm {name} cut nothing");
        } else {
            assert_eq!(r.totals.gbody, 0, "arm {name}");
        }
    }
}

/// The census over a directory of generated ontologies named by `WC1220_CENSUS_DIR`, for
/// ranking files by how often they meet the cases of the open obligations. It prints one
/// line per file and arm and fails on a violation of L1 or L2.
#[test]
#[ignore]
fn census_over_a_directory() {
    let Some(dir) = std::env::var_os("WC1220_CENSUS_DIR") else {
        return;
    };
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "ofn"))
        .collect();
    files.sort();
    let (mut l1, mut l2) = (0usize, 0usize);
    for path in &files {
        let ax = axioms_of(path);
        for (name, arm) in ARMS {
            let r = classify(&ax, arm, false, false, Some(200_000));
            let t = &r.totals;
            l1 += t.flags.len();
            l2 += t.fc_body.len() + t.xy_bodies.len();
            println!(
                "CENSUS\t{}\t{name}\t{}\t{}\t{}\t{}\t{}\t{}",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                u8::from(r.complete),
                t.processed,
                t.gbody,
                t.fc_head_gbody_outside,
                t.gbody_in_query,
                t.flags.len() + t.fc_body.len() + t.xy_bodies.len()
            );
            for f in t.flags.iter().chain(&t.fc_body).chain(&t.xy_bodies) {
                println!("VIOLATION\t{}\t{name}\t{f}", path.display());
            }
        }
    }
    assert_eq!((l1, l2), (0, 0), "violations of L1 and L2");
}
