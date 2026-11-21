//! Row 1231's marker repair diagnostic: the deficits at quiescence find a missing marker
//! and a missing return product. The repair's test mode is global to the process, so this
//! binary holds one test, which runs its cases in turn.

use sparkles_owl::classify::{Classifier, Expansion, Limits, Tier, marker_test_mode};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;

const EX: &str = "http://example.org/";
/// A disjunction elsewhere, so that the run takes Tier D.
const UNION: &str = "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))";

fn axioms(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n{UNION}\n)"
    ))
    .unwrap()
    .axioms
}

/// The fixtures, as in `dl_nominals`'s REPAIRS: the regression, a link to the constant from
/// a deeper derivation, and a denial that returns as a contradiction.
const FIXTURES: [(&str, &str); 3] = [
    (
        "av2",
        "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))",
    ),
    (
        "late",
        "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E :E1)
     SubClassOf(:E1 ObjectSomeValuesFrom(:r5 :E2))
     SubClassOf(:E2 ObjectHasValue(:r6 :u))
     SubClassOf(ObjectSomeValuesFrom(:r6 ObjectOneOf(:u)) :E3)
     SubClassOf(ObjectSomeValuesFrom(:r5 :E3) :E4)
     SubClassOf(:E4 ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))",
    ),
    (
        "denial2",
        "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) owl:Nothing)",
    ),
];

/// The modes: the repair on (1), on without returns (1 + 32), on without the replay of
/// markers to contexts registered or reached late (1 + 64), and off.
const ON: u8 = 1;
const NO_RETURN: u8 = 1 | 32;
const NO_REPLAY: u8 = 1 | 64;
const OFF: u8 = 0;

/// Threads, expansion, and whether the run goes in steps; without steps a run of several
/// threads saturates on all of them.
type Cfg = (usize, Expansion, bool);
const CFGS: [Cfg; 4] = [
    (1, Expansion::Cautious, true),
    (1, Expansion::Eager, true),
    (4, Expansion::Cautious, false),
    (4, Expansion::Eager, false),
];

fn deficits(body: &str, mode: u8, (threads, expansion, dl_steps): Cfg) -> [usize; 3] {
    marker_test_mode(mode, true);
    let mut k = Classifier::of(&axioms(body));
    let out = k
        .classify(&Limits {
            threads,
            expansion,
            rooted: true,
            rooted_decline: false,
            rooted_marker_repair: mode != OFF,
            dl_steps,
            ..Limits::default()
        })
        .unwrap();
    marker_test_mode(0, false);
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    out.stats.dl_marker_deficits
}

/// The deficits of a saturation of `body` with the mode `mode`, and whether it found a model.
fn saturated(body: &str, mode: u8, (threads, expansion, dl_steps): Cfg) -> ([usize; 3], bool) {
    marker_test_mode(mode, true);
    let mut k = Classifier::of(&axioms(body));
    let out = k
        .saturate(&Limits {
            threads,
            expansion,
            rooted: true,
            rooted_decline: false,
            rooted_marker_repair: mode != OFF,
            dl_steps,
            ..Limits::default()
        })
        .unwrap();
    marker_test_mode(0, false);
    (out.stats.dl_marker_deficits, out.consistent)
}

/// With the repair each fixture is owed nothing; a run without returns is owed a return
/// product, and a run without the replay or without the repair is owed markers. A
/// saturation of the regression is owed markers without the repair and nothing with it.
#[test]
fn the_deficits_find_a_missing_marker_and_a_missing_return_product() {
    for (name, body) in FIXTURES {
        for cfg in CFGS {
            let at = format!("{name}, {cfg:?}");
            for _ in 0..4 {
                assert_eq!(deficits(body, ON, cfg), [0, 0, 0], "{at}: on");
            }
            let d = deficits(body, NO_RETURN, cfg);
            assert!(d[0] == 0 && d[1] > 0, "{at}: without returns {d:?}");
            let d = deficits(body, NO_REPLAY, cfg);
            assert!(d[0] > 0, "{at}: without the replay {d:?}");
            let d = deficits(body, OFF, cfg);
            assert!(d[0] > 0, "{at}: off {d:?}");
        }
    }
    // a saturation takes the same deficits: the regression's are owed markers without the
    // repair and nothing with it, though no answer of a saturation shows them
    for cfg in CFGS {
        let (on, model) = saturated(FIXTURES[0].1, ON, cfg);
        assert!(model && on == [0, 0, 0], "saturation, {cfg:?}: on {on:?}");
        let (off, _) = saturated(FIXTURES[0].1, OFF, cfg);
        assert!(off[0] > 0, "saturation, {cfg:?}: off {off:?}");
    }
}
