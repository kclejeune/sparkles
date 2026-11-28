//! The closure check of item 1229's lemma (F24, "Item 804's departures from the journal's
//! rules"). Pred takes each clause that a successor sends once, with the restrictions that
//! reach the successor when it comes, and a run that counts or has the root replays the
//! clauses for a restriction that reaches the successor later. The lemma says that after a
//! saturation that finished, Pred with every clause a successor sent and every restriction
//! that reaches it gives nothing that the context does not already hold, whatever the order
//! in which the clauses, the restrictions and the premises came. These tests saturate the
//! ontologies built around each order of arrival and run that check on every context, and
//! with the replay turned off the same check finds the inferences that the late
//! restrictions left out.

use super::*;
use std::cell::RefCell;

thread_local! {
    /// the findings of the check on the current thread, one list per saturation
    static AUDIT: RefCell<Option<Vec<Vec<dl_rules::PredAudit>>>> = const { RefCell::new(None) };
}

/// Whether a test on this thread observes the check.
pub(super) fn observing() -> bool {
    AUDIT.with(|a| a.borrow().is_some())
}

/// The check's finding for one context of the saturation that is ending.
pub(super) fn record(f: dl_rules::PredAudit) {
    AUDIT.with(|a| {
        if let Some(runs) = a.borrow_mut().as_mut() {
            if runs.is_empty() {
                runs.push(Vec::new());
            }
            runs.last_mut().expect("a saturation").push(f);
        }
    });
}

/// The end of a saturation that the check read.
pub(super) fn saturated() {
    AUDIT.with(|a| {
        if let Some(runs) = a.borrow_mut().as_mut() {
            runs.push(Vec::new());
        }
    });
}

/// What the check found in each saturation of one run.
struct Seen {
    /// per saturation, the clauses and units that Pred gave again and the first one's
    /// description
    runs: Vec<(usize, Option<String>)>,
    /// whether the run answered in full
    complete: bool,
    out: Classification,
}

fn audited(body: &str, counting: bool, rooted: bool, threads: usize, eager: bool) -> Seen {
    let ax = crate::ofn_read::parse(&format!(
        "Prefix(:=<http://example.org/s#>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n)"
    ))
    .expect("test ontology")
    .axioms;
    let mut k = Classifier::of(&ax);
    AUDIT.with(|a| *a.borrow_mut() = Some(Vec::new()));
    let out = k.classify(&Limits {
        threads,
        counting,
        rooted,
        expansion: if eager {
            Expansion::Eager
        } else {
            Expansion::Cautious
        },
        ..Limits::default()
    });
    let found = AUDIT.with(|a| a.borrow_mut().take()).expect("observed");
    let out = out.expect("a classification");
    let runs = found
        .into_iter()
        .filter(|r| !r.is_empty())
        .map(|r| {
            let n = r.iter().map(|f| f.clauses + f.units).sum();
            (n, r.into_iter().find_map(|f| f.first))
        })
        .collect();
    let complete = out.complete && k.excluded().is_empty() && !out.stats.dl_over_budget;
    Seen {
        runs,
        complete,
        out,
    }
}

/// Whether the run says `a ⊑ b` for two named classes.
fn sub(out: &Classification, a: &str, b: &str) -> bool {
    out.taxonomy.is_subclass(
        &Class::new(format!("http://example.org/s#{a}")),
        &Class::new(format!("http://example.org/s#{b}")),
    )
}

/// One ontology built around an order of arrival: the routes it runs on, the subsumption
/// that the late restriction gives, and the configurations whose run without the replay
/// leaves an inference out.
struct Case {
    name: &'static str,
    body: &'static str,
    counting: bool,
    rooted: bool,
    /// the subsumption that a complete answer holds
    want: (&'static str, &'static str),
    /// the configurations (eager or cautious, on one thread) whose run without the replay
    /// leaves out a Pred inference that the check sees in this harness
    lost_without: &'static [bool],
}

const CASES: &[Case] = &[
    // K's restriction t14 into {v} ⊓ B9 comes after D2's s7-successor {v} sent `x ≈ v`, so
    // the unit's own successor term takes the constant only from that clause (e558)
    Case {
        name: "a late unit into a successor that already answered",
        body: "Declaration(Class(:Ans))
             Declaration(Class(:B3))
             Declaration(Class(:B9))
             Declaration(Class(:D2))
             Declaration(Class(:F15))
             Declaration(Class(:F8))
             Declaration(Class(:G16))
             Declaration(Class(:K))
             Declaration(Class(:M10))
             Declaration(Class(:M11))
             Declaration(Class(:M12))
             Declaration(Class(:M13))
             Declaration(Class(:M5))
             Declaration(Class(:Q))
             Declaration(Class(:Q1))
             Declaration(Class(:Q2))
             Declaration(ObjectProperty(:r1))
             Declaration(ObjectProperty(:s7))
             Declaration(ObjectProperty(:t14))
             Declaration(ObjectProperty(:t4))
             Declaration(ObjectProperty(:t6))
             Declaration(NamedIndividual(:v))
             Declaration(NamedIndividual(:w))
             SubClassOf(:K ObjectSomeValuesFrom(:r1 :D2))
             SubClassOf(:K ObjectHasValue(:t4 :v))
             SubClassOf(:K ObjectAllValuesFrom(:t4 ObjectOneOf(:w)))
             SubClassOf(:K :M5)
             SubClassOf(:M5 ObjectSomeValuesFrom(:t6 ObjectIntersectionOf(ObjectOneOf(:w) :B3)))
             SubClassOf(:D2 ObjectSomeValuesFrom(:s7 ObjectOneOf(:v)))
             SubClassOf(ObjectSomeValuesFrom(:s7 :B3) :F8)
             SubClassOf(:K :M10)
             SubClassOf(:M10 :M11)
             SubClassOf(:M11 :M12)
             SubClassOf(:M12 :M13)
             SubClassOf(:M13 ObjectSomeValuesFrom(:t14 ObjectIntersectionOf(ObjectOneOf(:v) :B9)))
             SubClassOf(ObjectSomeValuesFrom(:s7 ObjectIntersectionOf(:B9 owl:Thing)) :F15)
             SubClassOf(ObjectIntersectionOf(:F8 :F15) :G16)
             SubClassOf(ObjectSomeValuesFrom(:r1 :G16) :Ans)
             SubClassOf(ObjectIntersectionOf(:Ans :M13) :D2)
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             Declaration(ObjectProperty(<urn:cnt:zz>))
             FunctionalObjectProperty(<urn:cnt:zz>)",
        counting: true,
        rooted: true,
        want: ("K", "F15"),
        lost_without: &[false, true],
    },
    // K's s-restriction comes after a case analysis, while its r-restriction holds at once,
    // and the at-most restriction over p makes their successors one (x2)
    Case {
        name: "a late unit that counting merges with an earlier successor",
        body: "SubObjectPropertyOf(:r :p)
             SubObjectPropertyOf(:s :p)
             SubClassOf(:C :B)
             SubClassOf(:K ObjectMaxCardinality(1 :p :B))
             SubClassOf(:K ObjectSomeValuesFrom(:r :C))
             SubClassOf(:K ObjectAllValuesFrom(:r :D))
             SubClassOf(:K ObjectSomeValuesFrom(:r1 :D2))
             SubClassOf(:D2 ObjectAllValuesFrom(ObjectInverseOf(:r1) :A))
             SubClassOf(:A ObjectSomeValuesFrom(:s :C))
             SubClassOf(ObjectSomeValuesFrom(:s :D) :Ans)
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))",
        counting: true,
        rooted: false,
        want: ("K", "Ans"),
        lost_without: &[false],
    },
    // under the eager strategy the r- and s-restrictions of K share the successor whose
    // core is C and D, and the s-edge comes after the successor sent its clauses
    Case {
        name: "a late edge into a shared eager successor",
        body: "SubClassOf(:A :L0)
             SubClassOf(:E ObjectSomeValuesFrom(:s :C))
             SubClassOf(:Y2 :Y)
             SubClassOf(:K ObjectSomeValuesFrom(:r1 ObjectUnionOf(:Y1 :Y2)))
             SubObjectPropertyOf(:r :p)
             SubObjectPropertyOf(:s :p)
             SubClassOf(:L0 :L1)
             FunctionalObjectProperty(:p)
             SubClassOf(:C ObjectIntersectionOf(:B :B2))
             SubClassOf(ObjectSomeValuesFrom(:r1 :Y) :A)
             SubClassOf(:K ObjectAllValuesFrom(:r :D))
             SubClassOf(:Y1 :Y)
             SubClassOf(:K ObjectSomeValuesFrom(:r :C))
             SubClassOf(ObjectSomeValuesFrom(:s :D) :Ans)
             SubClassOf(:L1 ObjectSomeValuesFrom(:s :C))",
        counting: true,
        rooted: false,
        want: ("K", "Ans"),
        lost_without: &[],
    },
    // the same late edge in a rooted run that counts, with K an individual's class
    Case {
        name: "a late edge into a shared eager successor of a rooted run",
        body: "SubClassOf(:A :L0)
             SubClassOf(:E ObjectSomeValuesFrom(:s :C))
             SubClassOf(:Y2 :Y)
             SubClassOf(:K ObjectSomeValuesFrom(:r1 ObjectUnionOf(:Y1 :Y2)))
             SubObjectPropertyOf(:r :p)
             SubObjectPropertyOf(:s :p)
             SubClassOf(:L0 :L1)
             FunctionalObjectProperty(:p)
             SubClassOf(:C ObjectIntersectionOf(:B :B2))
             SubClassOf(ObjectSomeValuesFrom(:r1 :Y) :A)
             SubClassOf(:K ObjectAllValuesFrom(:r :D))
             SubClassOf(:Y1 :Y)
             SubClassOf(:K ObjectSomeValuesFrom(:r :C))
             SubClassOf(ObjectSomeValuesFrom(:s :D) :Ans)
             SubClassOf(:L1 ObjectSomeValuesFrom(:s :C))
             ClassAssertion(:K :a)",
        counting: true,
        rooted: true,
        want: ("K", "Ans"),
        lost_without: &[],
    },
    // the same late edge in a rooted run without counting, with K ⊑ ∀s.D in place of the
    // functional property
    Case {
        name: "a late edge into a shared eager successor of a rooted run without counting",
        body: "SubClassOf(:A :L0)
             SubClassOf(:E ObjectSomeValuesFrom(:s :C))
             SubClassOf(:Y2 :Y)
             SubClassOf(:K ObjectSomeValuesFrom(:r1 ObjectUnionOf(:Y1 :Y2)))
             SubObjectPropertyOf(:r :p)
             SubObjectPropertyOf(:s :p)
             SubClassOf(:L0 :L1)
             SubClassOf(:C ObjectIntersectionOf(:B :B2))
             SubClassOf(ObjectSomeValuesFrom(:r1 :Y) :A)
             SubClassOf(:K ObjectAllValuesFrom(:r :D))
             SubClassOf(:Y1 :Y)
             SubClassOf(:K ObjectSomeValuesFrom(:r :C))
             SubClassOf(ObjectSomeValuesFrom(:s :D) :Ans)
             SubClassOf(:L1 ObjectSomeValuesFrom(:s :C))
             SubClassOf(:K ObjectAllValuesFrom(:s :D))
             ClassAssertion(:K :a)",
        counting: false,
        rooted: true,
        want: ("K", "Ans"),
        lost_without: &[true],
    },
];

/// Every context of every run of the cases is closed under Pred with the late rules on,
/// on one thread and on four with both strategies, and the run that answers in full holds
/// the case's subsumption.
#[test]
fn every_order_of_arrival_leaves_the_contexts_closed_under_pred() {
    for case in CASES {
        for (threads, eager) in [(1, false), (1, true), (4, false), (4, true)] {
            let seen = audited(case.body, case.counting, case.rooted, threads, eager);
            assert!(
                !seen.runs.is_empty(),
                "{}: no saturation checked",
                case.name
            );
            for (n, first) in &seen.runs {
                assert_eq!(
                    *n, 0,
                    "{} on {threads} threads, eager {eager}: Pred gives again {first:?}",
                    case.name
                );
            }
            if seen.complete {
                let (a, b) = case.want;
                assert!(
                    sub(&seen.out, a, b),
                    "{}: {a} ⊑ {b} is missing on {threads} threads, eager {eager}",
                    case.name
                );
            }
        }
    }
}

/// The check sees the losses: with the replay off, which `SPARKLES_COUNTING_LATE_UNIT=0`
/// gives, the contexts of each case are not closed under Pred on one thread.
#[test]
fn without_the_replay_the_check_finds_the_inferences_left_out() {
    let mut missed = Vec::new();
    for case in CASES {
        for &eager in case.lost_without {
            let seen = dl_rules::with_late_unit_mode_override(0, || {
                audited(case.body, case.counting, case.rooted, 1, eager)
            });
            if !seen.runs.iter().any(|(n, _)| *n > 0) {
                missed.push(format!("{}, eager {eager}", case.name));
            }
        }
    }
    assert!(
        missed.is_empty(),
        "the check finds nothing without the replay: {missed:?}"
    );
}
