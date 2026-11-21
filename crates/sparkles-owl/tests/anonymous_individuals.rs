//! Anonymous individuals in the classifier (roadmap item 1281, F24 "Anonymous
//! individuals"): each one is a fresh individual of the run, which the consistency of the
//! ontology accounts for and which no answer names. The expected answers come from the
//! semantics of OWL 2, under which an ontology holds when some element stands for each
//! anonymous individual, so the entailments that do not name one are those of the
//! ontology with a fresh named individual in its place.

use sparkles_owl::classify::{Classification, Classifier, Excluded, Limits, Tier};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;

const EX: &str = "http://ex.org/";

fn c(s: &str) -> Class {
    Class::new(format!("{EX}{s}"))
}

fn axioms(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\nOntology(\n{body}\n)"
    ))
    .unwrap()
    .axioms
}

/// Classifies on one and eight threads, with and without the rooted run, checks that the
/// runs agree on consistency and leave nothing out, and returns the default run.
fn run(body: &str) -> (Classification, Vec<Excluded>) {
    let ax = axioms(body);
    let mut outs = Vec::new();
    for rooted in [false, true] {
        for threads in [1, 8] {
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads,
                    rooted,
                    ..Limits::default()
                })
                .unwrap();
            outs.push((out, k.excluded()));
        }
    }
    for (o, ex) in &outs[1..] {
        assert_eq!(o.consistent, outs[0].0.consistent, "the runs disagree");
        assert_eq!(*ex, outs[0].1, "the runs leave out different axioms");
    }
    outs.swap_remove(0)
}

/// The answer of a run that takes every axiom.
fn whole(body: &str) -> Classification {
    let (out, ex) = run(body);
    assert!(ex.is_empty(), "{ex:#?}");
    out
}

#[test]
fn a_class_assertion_makes_its_class_non_empty() {
    let out = whole(
        "DisjointClasses(:A :B) SubClassOf(:C :A) SubClassOf(:C :B)
         ClassAssertion(:C _:x)",
    );
    assert!(!out.consistent);
    let out = whole(
        "DisjointClasses(:A :B) SubClassOf(:C :A) ClassAssertion(:C _:x)
         SubClassOf(:D :E)",
    );
    assert!(out.consistent);
    assert!(out.taxonomy.is_subclass(&c("C"), &c("A")));
    assert!(out.taxonomy.is_subclass(&c("D"), &c("E")));
    assert!(!out.taxonomy.is_subclass(&c("A"), &c("C")));
    assert_eq!(out.stats.tier, Tier::El);
}

#[test]
fn a_property_assertion_links_an_anonymous_individual() {
    // _:x is an A with an r-successor a that is a B, so _:x is a D, which A excludes
    let out = whole(
        "ClassAssertion(:A _:x) ObjectPropertyAssertion(:r _:x :a) ClassAssertion(:B :a)
         SubClassOf(ObjectSomeValuesFrom(:r :B) :D) DisjointClasses(:A :D)",
    );
    assert!(!out.consistent);
    // the same with the anonymous individual as the object
    let out = whole(
        "ClassAssertion(:B _:y) ObjectPropertyAssertion(:r :a _:y)
         SubClassOf(ObjectSomeValuesFrom(:r :B) :D) ClassAssertion(:A :a)
         DisjointClasses(:A :D)",
    );
    assert!(!out.consistent);
    // two anonymous individuals, with a negative assertion that the positive one breaks
    let out =
        whole("ObjectPropertyAssertion(:r _:x _:y) NegativeObjectPropertyAssertion(:r _:x _:y)");
    assert!(!out.consistent);
}

#[test]
fn a_data_assertion_about_an_anonymous_individual_counts() {
    let out = whole(
        "DataPropertyAssertion(:d _:x \"1\"^^xsd:integer)
         SubClassOf(DataHasValue(:d \"1\"^^xsd:integer) owl:Nothing)",
    );
    assert!(!out.consistent);
    let out = whole(
        "DataPropertyAssertion(:d _:x \"1\"^^xsd:integer)
         NegativeDataPropertyAssertion(:d _:x \"1\"^^xsd:integer)",
    );
    assert!(!out.consistent);
}

#[test]
fn equality_and_difference_take_anonymous_members() {
    let out = whole(
        "SameIndividual(_:x :a) ClassAssertion(:A _:x) ClassAssertion(:B :a)
         DisjointClasses(:A :B)",
    );
    assert!(!out.consistent);
    let out = whole("SameIndividual(_:x :a) DifferentIndividuals(_:x :a)");
    assert!(!out.consistent);
    let out = whole("DifferentIndividuals(_:x :a) ClassAssertion(:A _:x) ClassAssertion(:A :a)");
    assert!(out.consistent);
}

#[test]
fn the_horn_rules_take_assertions_about_anonymous_individuals() {
    // ∀r.B of _:x reaches its r-successor a, which is not a B
    let out = whole(
        "ClassAssertion(ObjectAllValuesFrom(:r :B) _:x) ObjectPropertyAssertion(:r _:x :a)
         ClassAssertion(ObjectComplementOf(:B) :a)",
    );
    assert!(!out.consistent);
    assert_eq!(out.stats.tier, Tier::Horn);
    let out = whole(
        "ClassAssertion(ObjectAllValuesFrom(:r :B) _:x) ObjectPropertyAssertion(:r _:x :a)
         SubClassOf(:B :C)",
    );
    assert!(out.consistent);
}

#[test]
fn an_anonymous_individual_in_an_enumeration_is_a_fresh_individual() {
    // A is {_:x}, and _:x is a B, so A is below B
    let out = whole(
        "EquivalentClasses(:A ObjectOneOf(_:x)) ClassAssertion(:B _:x)
         SubClassOf(:C ObjectHasValue(:r _:x)) SubClassOf(ObjectSomeValuesFrom(:r :B) :D)",
    );
    assert!(out.consistent);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("B")));
    assert!(out.taxonomy.is_subclass(&c("C"), &c("D")));
}

#[test]
fn a_key_does_not_bind_an_anonymous_individual() {
    // a key binds named individuals alone, so _:x and a, which share the key's value,
    // need not be the same
    let anonymous = whole(
        "HasKey(:K (:p) ()) ClassAssertion(:K _:x) ObjectPropertyAssertion(:p _:x :v)
         ClassAssertion(:K :a) ObjectPropertyAssertion(:p :a :v)
         ClassAssertion(:A _:x) ClassAssertion(:B :a) DisjointClasses(:A :B)",
    );
    assert!(anonymous.consistent);
    // with a named individual in its place, the key makes them equal
    let named = whole(
        "HasKey(:K (:p) ()) ClassAssertion(:K :x) ObjectPropertyAssertion(:p :x :v)
         ClassAssertion(:K :a) ObjectPropertyAssertion(:p :a :v)
         ClassAssertion(:A :x) ClassAssertion(:B :a) DisjointClasses(:A :B)",
    );
    assert!(!named.consistent);
}

#[test]
fn no_answer_names_an_anonymous_individual() {
    let ax = axioms("ClassAssertion(:A _:x) ClassAssertion(:A :a) SubClassOf(:A :B)");
    let mut k = Classifier::of(&ax);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.consistent);
    assert!(k.excluded().is_empty());
    let d = k.describe(&c("B")).unwrap();
    assert_eq!(d.instances, vec![NamedIndividual::new(format!("{EX}a"))]);
    // in an inconsistent ontology every individual is an instance, the named ones alone
    let ax = axioms("ClassAssertion(:A _:x) ClassAssertion(:B :a) SubClassOf(:A owl:Nothing)");
    let mut k = Classifier::of(&ax);
    assert!(!k.classify(&Limits::default()).unwrap().consistent);
    let d = k.describe(&c("B")).unwrap();
    assert_eq!(d.instances, vec![NamedIndividual::new(format!("{EX}a"))]);
}

#[test]
fn an_edit_of_an_anonymous_assertion_reaches_consistency() {
    let base =
        axioms("DisjointClasses(:A :B) SubClassOf(:C :A) SubClassOf(:C :B) SubClassOf(:D :E)");
    let assertion = axioms("ClassAssertion(:C _:x)");
    let mut k = Classifier::of(&base);
    assert!(k.classify(&Limits::default()).unwrap().consistent);
    k.add(&assertion);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.consistent);
    assert!(k.excluded().is_empty());
    k.remove(&assertion);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.consistent);
    assert!(out.taxonomy.is_subclass(&c("D"), &c("E")));
}

#[test]
fn an_anonymous_individual_keeps_the_disjunctive_rules_from_a_run_that_is_not_rooted() {
    // The disjunctive rules without the rooted run take no individuals, anonymous ones
    // among them, so that run leaves the disjunction out and the rooted run takes it.
    let body = "SubClassOf(:A ObjectUnionOf(:B :C)) SubClassOf(:B :D) SubClassOf(:C :D)
                ClassAssertion(:E _:x)";
    let ax = axioms(body);
    let mut k = Classifier::of(&ax);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
    assert_eq!(k.excluded().len(), 1);
    assert!(matches!(k.excluded()[0].axiom, AxiomKind::SubClassOf(..)));
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            rooted: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
}

#[test]
fn interchangeable_anonymous_individuals_share_one() {
    // a class assertion whose anonymous individual occurs nowhere else says only that its
    // class has an instance, so a thousand of them over one class hold the contexts of one
    let many: String = (0..1000)
        .map(|i| format!("ClassAssertion(:C _:x{i})\n"))
        .collect();
    let one = whole("SubClassOf(:C :D) ClassAssertion(:C _:x0)");
    let out = whole(&format!("SubClassOf(:C :D) {many}"));
    assert!(out.consistent);
    assert_eq!(out.stats.contexts, one.stats.contexts);
    assert_eq!(out.stats.added, 1001);
    // an individual of the assertions that occurs elsewhere keeps its own
    let out = whole(&format!(
        "SubClassOf(:C :D) {many} ClassAssertion(:E _:x7) DisjointClasses(:D :E)"
    ));
    assert!(!out.consistent);
    let out = whole(&format!(
        "SubClassOf(:C :D) {many} ObjectPropertyAssertion(:r :a _:x3)
         SubClassOf(ObjectSomeValuesFrom(:r :D) :F) ClassAssertion(:G :a) DisjointClasses(:F :G)"
    ));
    assert!(!out.consistent);
    let out = whole(&format!("SubClassOf(:C owl:Nothing) {many}"));
    assert!(!out.consistent);
}

fn rooted() -> Limits {
    Limits {
        rooted: true,
        ..Limits::default()
    }
}

#[test]
fn an_anonymous_individual_outside_the_el_part_makes_the_run_rooted() {
    // the union holds of _:x, and both of its members are empty
    let ax = axioms(
        "ClassAssertion(ObjectUnionOf(:A :B) _:x)
         SubClassOf(:A owl:Nothing) SubClassOf(:B owl:Nothing)",
    );
    for threads in [1, 8] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                ..rooted()
            })
            .unwrap();
        assert!(!out.consistent);
        assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    }
    // anonymous individuals in enumerations and values outside OWL 2 EL
    let ax = axioms(
        "SubClassOf(:A ObjectUnionOf(:B :C)) SubClassOf(:B :D) SubClassOf(:C :D)
         SubClassOf(:E ObjectOneOf(_:x _:y)) SubClassOf(:X ObjectHasValue(ObjectInverseOf(:r) _:x))",
    );
    let mut k = Classifier::of(&ax);
    let out = k.classify(&rooted()).unwrap();
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    // a run that counts: three r-successors among two individuals cannot be
    let ax = axioms(
        "SubClassOf(:A ObjectMinCardinality(3 :r ObjectOneOf(_:x _:y)))
         SubClassOf(:X ObjectUnionOf(:Y :Z))",
    );
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            counting: true,
            ..rooted()
        })
        .unwrap();
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
}

#[test]
fn an_assertion_left_out_reports_the_assertions_it_stands_for() {
    // the classifier shares an individual only among assertions of named classes, which
    // every run takes, so each assertion of a union that the EL rules leave out is
    // reported
    let ax =
        axioms("ClassAssertion(ObjectUnionOf(:A :B) _:x) ClassAssertion(ObjectUnionOf(:A :B) _:y)");
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            el_only: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(out.consistent);
    assert_eq!(k.excluded().len(), 2, "{:#?}", k.excluded());
    assert_eq!(out.stats.outside_el.disjunctive, 2);
    for (body, el_only) in [
        (
            "ClassAssertion(ObjectAllValuesFrom(:r :B) _:x) ClassAssertion(ObjectAllValuesFrom(:r :B) _:y)",
            true,
        ),
        (
            "ClassAssertion(DataHasValue(:d \"1\"^^:custom) _:x) ClassAssertion(DataHasValue(:d \"1\"^^:custom) _:y)",
            false,
        ),
    ] {
        let ax = axioms(body);
        let mut k = Classifier::of(&ax);
        k.classify(&Limits {
            el_only,
            ..Limits::default()
        })
        .unwrap();
        assert_eq!(k.excluded().len(), 2, "{body}: {:#?}", k.excluded());
    }
}

#[test]
fn a_run_that_starts_again_counts_every_axiom_as_added() {
    let ax = axioms(
        "ClassAssertion(:E _:x) ClassAssertion(:E _:y)
         SubClassOf(:A ObjectUnionOf(:B :C)) SubClassOf(:B :D) SubClassOf(:C :D)",
    );
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            dl_work: Some(1),
            ..rooted()
        })
        .unwrap();
    assert_eq!(out.stats.added, 5);
    let mut k = Classifier::of(&ax);
    assert_eq!(k.classify(&Limits::default()).unwrap().stats.added, 5);
    // item 805's rewriting of a key, which the run reads beside the shared individuals
    let many: String = (0..50)
        .map(|i| format!("ClassAssertion(:E _:x{i})\n"))
        .collect();
    let ax = axioms(&format!(
        "HasKey(:K (:p) ()) SubClassOf(:A ObjectUnionOf(:B :C)) SubClassOf(:B :D)
         SubClassOf(:C :D) {many}"
    ));
    let mut k = Classifier::of(&ax);
    assert_eq!(k.classify(&rooted()).unwrap().stats.added, 54);
}

/// Classifies `base`, applies `edit`, stops the next run at its first poll, and returns
/// the run after it, which must answer for the edit (review round 2 of item 1281: the
/// scan for shared individuals polls before the run is marked as needing a full run).
fn after_a_cancelled_run(base: &str, edit: impl FnOnce(&mut Classifier)) -> Classification {
    let base = axioms(base);
    let mut k = Classifier::of(&base);
    k.classify(&Limits::default()).unwrap();
    edit(&mut k);
    let stopped = k.classify(&Limits {
        cancelled: Some(std::sync::Arc::new(|| true)),
        ..Limits::default()
    });
    assert!(matches!(
        stopped,
        Err(sparkles_owl::classify::Error::Cancelled)
    ));
    k.classify(&Limits::default()).unwrap()
}

#[test]
fn a_cancelled_run_keeps_an_added_axiom() {
    for extra in ["", "ClassAssertion(:D _:x) ClassAssertion(:D _:y)"] {
        let add = axioms("SubClassOf(:B :C)");
        let out = after_a_cancelled_run(
            &format!("SubClassOf(:A :B) Declaration(Class(:C)) {extra}"),
            |k| k.add(&add),
        );
        assert!(out.taxonomy.is_subclass(&c("A"), &c("C")), "{extra}");
    }
}

#[test]
fn a_cancelled_run_drops_a_removed_axiom() {
    for extra in ["", "ClassAssertion(:D _:x) ClassAssertion(:D _:y)"] {
        let gone = axioms("SubClassOf(:B :C)");
        let out = after_a_cancelled_run(
            &format!("SubClassOf(:A :B) SubClassOf(:B :C) Declaration(Class(:C)) {extra}"),
            |k| k.remove(&gone),
        );
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("C")), "{extra}");
    }
}

#[test]
fn the_pool_grants_no_more_than_the_peak_with_shared_individuals() {
    // review round 2 of item 1281: what the pool admitted for the scan for shared
    // individuals is what the run draws on first, and the peak counts it
    let body: String = (0..10_000)
        .map(|i| format!("ClassAssertion(:C _:x{i})\n"))
        .collect();
    let ax = axioms(&body);
    let kinds: Vec<&AxiomKind> = ax.iter().map(|a| &a.kind).collect();
    let granted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let g = granted.clone();
    let limits = Limits {
        threads: 1,
        charge: Some(std::sync::Arc::new(move |b: usize| {
            g.fetch_add(b, std::sync::atomic::Ordering::Relaxed);
            true
        })),
        ..Limits::default()
    };
    let mut sat = sparkles_owl::classify::Saturation::new();
    let out = sat.classify(&kinds, None, &limits).unwrap();
    let granted = granted.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        granted <= out.stats.peak_memory,
        "granted {granted}, peak {}",
        out.stats.peak_memory
    );
}
