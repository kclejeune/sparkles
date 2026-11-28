//! Containment of protections whose patterns the caller and the view evaluate apart:
//! a pattern that reads the clock, and a caller whose guards are decided at each
//! snapshot (spec F13 §6.1).

use super::*;
use crate::access::{Decider, GuardDecision};

const DATED: &str = r#"@prefix : <http://ex/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
:a :age 30 ; :until "2030-01-01T00:00:00Z"^^xsd:dateTime ; :owner "alice" .
:b :age 40 ; :owner "bob" .
"#;
const AGES: &str = "SELECT ?s ?a WHERE { ?s <http://ex/age> ?a }";
const GUARDED: &str = "urn:x-test:guarded/*";

fn dated() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[crate::io::Source::from_bytes(
        DATED.as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .expect("load");
    s
}

fn alice() -> Caller {
    Caller {
        user: Some("alice".into()),
        ..Default::default()
    }
}

fn define_ages(rt: &Runtime, s: &Store, principal: &str) {
    let mut def = ViewDef::table(AGES.to_string());
    def.principal = Some(principal.into());
    rt.put(s, "ages", def).unwrap();
    rt.round(s);
}

fn direct_rows(s: &Store, caller: &Arc<GraphAccess>) -> usize {
    crate::sparql::query(
        s.snapshot(),
        AGES,
        &crate::sparql::QueryOptions {
            graphs: Some(caller.clone()),
            no_cache: true,
            ..Default::default()
        },
    )
    .expect("query")
    .test_application_rows()
    .len()
}

fn instant(t: &str) -> oxsdatatypes::DateTime {
    t.parse().expect("a dateTime")
}

/// A pattern that reads the clock lets other triples through on the same commit at
/// another time, so a caller with the view's own protection and attributes is not
/// contained, while a lift of its own, unrestricted access and a grant that names the
/// view still read it.
#[test]
fn a_pattern_that_reads_the_clock_is_not_contained() {
    let s = dated();
    let clock = protection(
        "current",
        Some("?s <http://ex/until> ?t FILTER(?t > NOW())"),
    );
    let snap = s.snapshot();
    let passing = |t: &str| {
        crate::sparql::ctx::at_test_clock(instant(t), || {
            match crate::access::triples::passes(&snap, &clock, &alice(), &Limits::default())
                .expect("pattern")
            {
                crate::access::triples::Passes::Keys { set, .. } => set.len(),
                crate::access::triples::Passes::All => usize::MAX,
                crate::access::triples::Passes::None => 0,
            }
        })
    };
    assert_eq!(passing("2029-06-01T00:00:00Z"), 1, "before the date");
    assert_eq!(
        passing("2031-06-01T00:00:00Z"),
        0,
        "after it, on the same commit"
    );

    let principal = protected(&["default"], &clock, Graphs::none(), alice());
    let caller = protected(&["default"], &clock, Graphs::none(), alice());
    let policy = Policy::with(&[("dated", Some(principal))]);
    let rt = runtime(&s, &policy);
    define_ages(&rt, &s, "dated");
    let decided = rt
        .authorize_view("ages", Some(&caller), ViewOp::Read)
        .unwrap();
    assert!(!decided.allowed(), "{decided:?}");
    assert_eq!(decided.reason(), "evaluation");
    assert_eq!(
        code(&read(&rt, &s, "ages", &caller).unwrap_err()),
        "view-access"
    );
    assert!(
        !covers(Some(&caller), Some(&caller)),
        "not even against itself"
    );

    // the view's own rows, as an unrestricted reader reads them
    let rows = read_at(&rt, &s, "ages", None, None).unwrap();
    assert_eq!(rows, 1);
    let lifted = protected(&["default"], &clock, Graphs::All, alice());
    assert_eq!(read(&rt, &s, "ages", &lifted).unwrap(), rows);
    let named = Arc::new(GraphAccess {
        views: ViewGrants::new(["ages".to_string()], Some("carol")),
        ..(*caller).clone()
    });
    assert_eq!(
        rt.authorize_view("ages", Some(&named), ViewOp::Read)
            .unwrap(),
        ViewDecision::Allow(super::super::ViewAllow::Declassified)
    );
}

/// A pattern whose result depends on the snapshot and the caller alone is contained
/// for the same caller, and the caller's own read holds every row of the view.
#[test]
fn a_stable_pattern_that_binds_the_caller_is_contained() {
    let s = dated();
    let own = protection("own", Some("?s <http://ex/owner> ?user"));
    let principal = protected(&["default"], &own, Graphs::none(), alice());
    let caller = protected(&["default"], &own, Graphs::none(), alice());
    let policy = Policy::with(&[("own", Some(principal))]);
    let rt = runtime(&s, &policy);
    define_ages(&rt, &s, "own");
    assert_eq!(
        rt.authorize_view("ages", Some(&caller), ViewOp::Read)
            .unwrap(),
        ViewDecision::Allow(super::super::ViewAllow::Contained)
    );
    let rows = read(&rt, &s, "ages", &caller).unwrap();
    assert_eq!(rows, 1);
    assert_eq!(direct_rows(&s, &caller), rows);
}

struct NoExemption;

impl GuardDecision for NoExemption {
    fn exempt(&self, _: &Arc<crate::store::Snapshot>) -> Vec<String> {
        Vec::new()
    }
}

/// Grant sets whose graphs and protections are equal once their guards are closed,
/// with the guard fixed by the grants (`fixed`), decided at each snapshot
/// (`deciding`) and decided at one (`bound`). A pattern protection is in force.
fn guarded_callers(s: &Store) -> [Arc<GraphAccess>; 3] {
    let own = protection("own", Some("?s <http://ex/owner> ?user"));
    let with = |read: Graphs| {
        Arc::new(GraphAccess::with_triples(
            read,
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: own.clone(),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: alice(),
                limits: Limits::default(),
            },
        ))
    };
    let fixed = with(rule(&["default"]).guarded(GUARDED, &[]));
    let deciding = with(rule(&["default"]).guarded_by(GUARDED, Decider(Arc::new(NoExemption))));
    let bound = deciding.bound(&s.snapshot());
    [fixed, deciding, bound]
}

/// A caller whose guards are decided at each snapshot refuses evidence that a view
/// with the same guards fixed accepts while a pattern protection is in force, so it is
/// not contained, whichever of the two the cache decides first. One whose guard is
/// fixed is contained.
#[test]
fn a_caller_whose_guards_bind_is_not_contained_under_a_pattern() {
    let s = dated();
    let [fixed, deciding, bound] = guarded_callers(&s);
    assert_eq!(fixed.guards_closed(), deciding.guards_closed());
    assert_eq!(fixed.guards_closed(), bound.guards_closed());
    for order in [
        [&fixed, &deciding, &bound],
        [&bound, &deciding, &fixed],
        [&deciding, &fixed, &bound],
    ] {
        let policy = Policy::with(&[("own", Some(fixed.clone()))]);
        let rt = runtime(&s, &policy);
        define_ages(&rt, &s, "own");
        for c in order {
            let allowed = Arc::ptr_eq(c, &fixed);
            for _ in 0..2 {
                let d = rt.authorize_view("ages", Some(c), ViewOp::Read).unwrap();
                assert_eq!(d.allowed(), allowed, "{d:?}");
                if !allowed {
                    assert_eq!(d.reason(), "evaluation");
                }
                assert_eq!(rt.readable_by("ages", Some(c)), allowed);
                assert_eq!(read(&rt, &s, "ages", c).is_ok(), allowed);
            }
        }
    }
    assert!(covers(Some(&fixed), Some(&fixed)));
    assert!(!covers(Some(&deciding), Some(&fixed)));
    assert!(!covers(Some(&bound), Some(&fixed)));
    // without a pattern in force, a guard decided at each snapshot is contained
    let plain = |g: Graphs| Arc::new(GraphAccess::graphs(g, Graphs::none()));
    assert!(covers(
        Some(&plain(
            rule(&["default"]).guarded_by(GUARDED, Decider(Arc::new(NoExemption)))
        )),
        Some(&plain(rule(&["default"]).guarded(GUARDED, &[]))),
    ));
}
