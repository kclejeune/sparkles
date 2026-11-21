//! The answer of a run that leaves axioms out (roadmap item 1271): it is not complete for
//! the whole ontology, the classes whose bottom-locality module holds a left-out axiom are
//! incomplete and the others have their complete answer, and a strict run refuses it.

use sparkles_owl::classify::{Classifier, Consistency, Error, Limits};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;

fn axioms(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!(
        "Prefix(:=<http://e/#>)\nOntology(<http://e/o>\n{body}\n)"
    ))
    .unwrap()
    .axioms
}

fn c(n: &str) -> Class {
    Class::new(format!("http://e/#{n}"))
}

/// The ontology where a run without the rooted rules misses K ⊑ Ans: the union S ⊑ D1 ⊔ D2
/// is left out by the rules that do not take it beside the assertion.
const IRL1: &str = "
Declaration(Class(:K)) Declaration(Class(:S)) Declaration(Class(:D1))
Declaration(Class(:D2)) Declaration(Class(:Ans))
SubObjectPropertyOf(:r :s)
SubClassOf(:K ObjectSomeValuesFrom(:r :S))
SubClassOf(:S ObjectUnionOf(:D1 :D2))
SubClassOf(:D1 ObjectAllValuesFrom(ObjectInverseOf(:s) :Ans))
SubClassOf(:D2 ObjectAllValuesFrom(ObjectInverseOf(:s) :Ans))
ClassAssertion(:K :a)
";

/// A run that leaves the union of C ⊓ D out misses A ⊑ H.
const M1: &str = "
Declaration(Class(:A)) Declaration(Class(:C)) Declaration(Class(:D))
Declaration(Class(:E)) Declaration(Class(:F)) Declaration(Class(:G))
Declaration(Class(:H))
SubClassOf(:A ObjectSomeValuesFrom(:r :C))
SubClassOf(:A ObjectAllValuesFrom(:r :D))
SubClassOf(ObjectIntersectionOf(:C :D) ObjectUnionOf(:E :F))
SubClassOf(:E :G)
SubClassOf(:F :G)
SubClassOf(ObjectSomeValuesFrom(:r :G) :H)
SubClassOf(:A ObjectSomeValuesFrom(:s ObjectOneOf(:u)))
";

#[test]
fn a_run_that_leaves_an_axiom_out_is_not_complete_and_names_the_classes_it_reaches() {
    for (body, missed, incomplete) in [
        (IRL1, ("K", "Ans"), vec![c("K"), c("S")]),
        (M1, ("A", "H"), vec![c("A")]),
    ] {
        let mut k = Classifier::of(&axioms(body));
        let out = k.classify(&Limits::default()).unwrap();
        assert!(!k.excluded().is_empty());
        assert!(!out.taxonomy.is_subclass(&c(missed.0), &c(missed.1)));
        assert!(!out.complete);
        assert!(out.complete_for_taken);
        assert_eq!(out.consistency, Consistency::Consistent);
        // S's module holds the union, though S's own answer does not depend on it
        assert_eq!(out.incomplete, incomplete);
    }
}

#[test]
fn a_run_that_takes_everything_is_complete() {
    let mut k = Classifier::of(&axioms("SubClassOf(:A :B) SubClassOf(:B :C)"));
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.complete && out.complete_for_taken && out.incomplete.is_empty());
    let strict = k
        .classify(&Limits {
            strict: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(strict.complete);
}

#[test]
fn a_strict_run_refuses_to_leave_an_axiom_out() {
    let mut k = Classifier::of(&axioms(M1));
    let e = k
        .classify(&Limits {
            strict: true,
            ..Limits::default()
        })
        .unwrap_err();
    let Error::Incomplete(excluded) = &e else {
        panic!("{e:?}");
    };
    assert_eq!(excluded.as_slice(), k.excluded().as_slice());
    let m = e.to_string();
    assert!(
        m.contains("leaves out or uses only in part 1 axiom:"),
        "{m}"
    );
    assert!(
        m.contains("ObjectUnionOf(:E :F)")
            || m.contains("ObjectUnionOf(<http://e/#E> <http://e/#F>)"),
        "{m}"
    );
    assert!(m.contains("OWL 2 Profiles"), "{m}");
    // the classifier keeps the run, and a run that is not strict answers
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.complete);
    // so does a saturation of the caller's axioms
    let ax = axioms(M1);
    let kinds: Vec<&AxiomKind> = ax.iter().map(|a| &a.kind).collect();
    let mut s = sparkles_owl::classify::Saturation::new();
    let e = s
        .classify(
            &kinds,
            None,
            &Limits {
                strict: true,
                ..Limits::default()
            },
        )
        .unwrap_err();
    assert!(matches!(e, Error::Incomplete(_)));
    let out = s.classify(&kinds, None, &Limits::default()).unwrap();
    assert_eq!(out.incomplete, [c("A")]);
}

#[test]
fn an_el_only_run_reports_its_coverage() {
    // the union is left out, and only A's module holds it
    let mut k = Classifier::of(&axioms(
        "SubClassOf(:A ObjectUnionOf(:B :C)) SubClassOf(:B :D) SubClassOf(:C :D) SubClassOf(:X :Y)",
    ));
    let out = k
        .classify(&Limits {
            el_only: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(!out.complete);
    assert_eq!(out.incomplete, [c("A")]);
    assert!(out.taxonomy.is_subclass(&c("X"), &c("Y")));
}
