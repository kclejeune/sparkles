//! The Horn rules of roadmap item 603 (Tier H): each rule of Kazakov's calculus for
//! Horn-SHIQ, the role constructs beside it and their interactions, on ontologies with
//! the entailments and non-entailments written from the semantics.

use sparkles_owl::classify::{Classification, Classifier, Limits, Tier};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;

const EX: &str = "http://ex.org/";

fn c(s: &str) -> Class {
    Class::new(format!("{EX}{s}"))
}

/// The axioms of a functional-syntax body with the prefix `:` for `http://ex.org/`.
fn axioms(body: &str) -> Vec<Axiom> {
    let doc = ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n)"
    ))
    .unwrap();
    doc.axioms
}

/// Classifies at one and at eight threads, checks that the answers are the same, and
/// returns one with the axioms left out.
fn run(body: &str) -> (Classification, Vec<sparkles_owl::classify::Excluded>) {
    let ax = axioms(body);
    let mut outs = Vec::new();
    for threads in [1, 8] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                ..Limits::default()
            })
            .unwrap();
        outs.push((out, k.excluded()));
    }
    let (b, _) = outs.pop().unwrap();
    let (a, ex) = outs.pop().unwrap();
    let pairs = |o: &Classification| -> Vec<(String, String)> {
        let t = &o.taxonomy;
        let cs: Vec<Class> = t.classes().cloned().collect();
        let mut v = Vec::new();
        for x in &cs {
            for y in &cs {
                if t.is_subclass(x, y) {
                    v.push((x.as_str().to_string(), y.as_str().to_string()));
                }
            }
        }
        v.sort();
        v
    };
    assert_eq!(pairs(&a), pairs(&b), "the answer depends on the threads");
    (a, ex)
}

/// Asserts that the whole ontology is taken and that each pair is entailed or not.
fn check(body: &str, yes: &[(&str, &str)], no: &[(&str, &str)]) -> Classification {
    let (out, ex) = run(body);
    assert!(ex.is_empty(), "left out: {ex:#?}");
    let t = &out.taxonomy;
    for (a, b) in yes {
        assert!(t.is_subclass(&c(a), &c(b)), "{a} ⊑ {b} is entailed");
    }
    for (a, b) in no {
        assert!(!t.is_subclass(&c(a), &c(b)), "{a} ⊑ {b} is not entailed");
    }
    out
}

fn unsat(body: &str, classes: &[&str], sat: &[&str]) {
    let (out, ex) = run(body);
    assert!(ex.is_empty(), "left out: {ex:#?}");
    for a in classes {
        assert!(out.taxonomy.is_unsatisfiable(&c(a)), "{a} is unsatisfiable");
    }
    for a in sat {
        assert!(!out.taxonomy.is_unsatisfiable(&c(a)), "{a} is satisfiable");
    }
}

#[test]
fn r4_a_universal_on_an_inverse_reaches_the_predecessor() {
    let out = check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:B ObjectAllValuesFrom(ObjectInverseOf(:r) :C))",
        &[("A", "C")],
        &[("B", "C")],
    );
    assert_eq!(out.stats.tier, Tier::Horn);
    assert_eq!(out.stats.outside_el.taken, 1);
}

#[test]
fn r3_a_universal_reaches_the_successor() {
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:A ObjectAllValuesFrom(:r :C))
         EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)))",
        &[("A", "D")],
        &[("B", "C"), ("D", "A")],
    );
}

#[test]
fn r3_through_a_superrole_and_nested_universals() {
    check(
        "SubObjectPropertyOf(:s :r)
         SubClassOf(:A ObjectSomeValuesFrom(:s ObjectSomeValuesFrom(:s :B)))
         SubClassOf(:A ObjectAllValuesFrom(:r ObjectAllValuesFrom(:r :C)))
         SubClassOf(ObjectIntersectionOf(:B :C) :E)
         SubClassOf(ObjectSomeValuesFrom(:s :E) :F)
         SubClassOf(ObjectSomeValuesFrom(:s :F) :G)",
        &[("A", "G")],
        &[("B", "E")],
    );
}

#[test]
fn a_universal_on_an_inverse_goes_back_two_steps() {
    // hand-horn-inverse-chain
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:B ObjectSomeValuesFrom(:r :C))
         SubClassOf(:C ObjectAllValuesFrom(ObjectInverseOf(:r) ObjectAllValuesFrom(ObjectInverseOf(:r) :E)))",
        &[("A", "E")],
        &[("B", "E"), ("C", "E")],
    );
}

#[test]
fn a_negative_existential_on_an_inverse_reaches_the_successor() {
    // an A's r-successor has an r⁻-successor in A, so it is B
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :C))
         SubClassOf(ObjectSomeValuesFrom(ObjectInverseOf(:r) :A) :B)
         SubClassOf(ObjectSomeValuesFrom(:r :B) :D)",
        &[("A", "D")],
        &[("C", "B")],
    );
}

#[test]
fn domains_and_ranges_of_inverses() {
    check(
        "ObjectPropertyDomain(ObjectInverseOf(:r) :X)
         ObjectPropertyRange(ObjectInverseOf(:s) :Y)
         SubClassOf(:A ObjectSomeValuesFrom(:r :C))
         SubClassOf(ObjectSomeValuesFrom(:r :X) :D)
         SubClassOf(:E ObjectSomeValuesFrom(:s :C))",
        &[("A", "D"), ("E", "Y")],
        &[("C", "X"), ("A", "Y")],
    );
}

#[test]
fn a_range_reaches_the_predecessor_of_an_inverse_link() {
    // A has an r⁻-successor, whose r-successor the A is, so A is in r's range
    check(
        "ObjectPropertyRange(:r :X)
         SubClassOf(:A ObjectSomeValuesFrom(ObjectInverseOf(:r) :C))",
        &[("A", "X")],
        &[("C", "X")],
    );
}

#[test]
fn symmetric_and_inverse_properties() {
    check(
        "SymmetricObjectProperty(:r)
         InverseObjectProperties(:s :t)
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:B ObjectAllValuesFrom(:r :C))
         SubClassOf(:E ObjectSomeValuesFrom(:s :F))
         SubClassOf(:F ObjectAllValuesFrom(:t :G))
         SubClassOf(:H ObjectSomeValuesFrom(:t :I))
         SubClassOf(:I ObjectAllValuesFrom(:s :J))",
        &[("A", "C"), ("E", "G"), ("H", "J")],
        &[("B", "C"), ("F", "G")],
    );
}

#[test]
fn a_universal_on_a_transitive_role_reaches_every_step() {
    // hand-forall-transitive
    check(
        "TransitiveObjectProperty(:r)
         SubClassOf(:A ObjectAllValuesFrom(:r :B))
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:r :C))))
         EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)))",
        &[("A", "D")],
        &[("C", "B")],
    );
}

#[test]
fn a_universal_on_a_chain_follows_its_automaton() {
    check(
        "SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubClassOf(:A ObjectAllValuesFrom(:t :B))
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s :C)))
         SubClassOf(ObjectIntersectionOf(:B :C) :E)
         SubClassOf(ObjectSomeValuesFrom(:s :E) :F)
         SubClassOf(ObjectSomeValuesFrom(:r :F) :G)",
        &[("A", "G")],
        &[("C", "E")],
    );
    // on the inverse of a chain, the universal goes back along it
    check(
        "SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s :C)))
         SubClassOf(:C ObjectAllValuesFrom(ObjectInverseOf(:t) :E))",
        &[("A", "E")],
        &[("C", "E")],
    );
}

#[test]
fn a_chain_with_an_inverse_operand() {
    // hand-inverse-chain
    check(
        "SubObjectPropertyOf(ObjectPropertyChain(ObjectInverseOf(:r) :s) :t)
         SubClassOf(:A ObjectSomeValuesFrom(ObjectInverseOf(:r) ObjectSomeValuesFrom(:s :B)))
         EquivalentClasses(:D ObjectSomeValuesFrom(:t :B))",
        &[("A", "D")],
        &[],
    );
}

#[test]
fn links_into_a_shared_filler_do_not_join() {
    // hand-shared-filler-no-join
    check(
        "SubObjectPropertyOf(ObjectPropertyChain(:r ObjectInverseOf(:s)) :t)
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:C ObjectSomeValuesFrom(:s :B))
         EquivalentClasses(:D ObjectSomeValuesFrom(:t :C))",
        &[],
        &[("A", "D")],
    );
}

#[test]
fn r5_a_functional_role_merges_its_successors() {
    // hand-functional-merge
    check(
        "FunctionalObjectProperty(:r)
         SubClassOf(:A ObjectIntersectionOf(ObjectSomeValuesFrom(:r :B) ObjectSomeValuesFrom(:r :C)))
         EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)))",
        &[("A", "D")],
        &[("B", "C")],
    );
}

#[test]
fn r5_a_qualified_at_most_merges_the_successors_in_its_filler() {
    // hand-max-qualified-merge
    check(
        "SubClassOf(:A ObjectMaxCardinality(1 :r :B))
         SubClassOf(:A ObjectSomeValuesFrom(:r :C))
         SubClassOf(:A ObjectSomeValuesFrom(:r :E))
         SubClassOf(:C :B)
         SubClassOf(:E :B)
         EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:C :E)))",
        &[("A", "D")],
        &[],
    );
    // a successor outside the filler stays apart
    check(
        "SubClassOf(:A ObjectMaxCardinality(1 :r :B))
         SubClassOf(:A ObjectSomeValuesFrom(:r :C))
         SubClassOf(:A ObjectSomeValuesFrom(:r :E))
         SubClassOf(:C :B)
         EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:C :E)))",
        &[],
        &[("A", "D")],
    );
}

#[test]
fn r5_with_a_universal_on_the_merged_successor() {
    check(
        "SubClassOf(:A ObjectMaxCardinality(1 :r owl:Thing))
         SubObjectPropertyOf(:s :r)
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:A ObjectSomeValuesFrom(:s :C))
         SubClassOf(:A ObjectAllValuesFrom(:s :E))
         SubClassOf(ObjectIntersectionOf(:B :E) :F)
         EquivalentClasses(:D ObjectSomeValuesFrom(:r :F))",
        &[("A", "D")],
        &[],
    );
}

#[test]
fn at_least_needs_distinct_successors() {
    // hand-max-clash
    unsat(
        "SubClassOf(:A ObjectMinCardinality(2 :r :B))
         SubClassOf(:A ObjectMaxCardinality(1 :r))",
        &["A"],
        &["B"],
    );
    unsat(
        "FunctionalObjectProperty(:r)
         SubObjectPropertyOf(:s :r)
         SubClassOf(:A ObjectMinCardinality(3 :s :B))
         SubClassOf(:E ObjectMinCardinality(1 :s :B))",
        &["A"],
        &["E", "B"],
    );
}

#[test]
fn r6_an_inverse_at_most_identifies_the_predecessor() {
    // hand-inverse-max
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:B ObjectMaxCardinality(1 ObjectInverseOf(:r) owl:Thing))
         SubClassOf(:B ObjectSomeValuesFrom(ObjectInverseOf(:r) :C))",
        &[("A", "C")],
        &[("B", "C"), ("C", "A")],
    );
    // hand-inverse-functional-merge
    check(
        "InverseFunctionalObjectProperty(:r)
         SubClassOf(:B ObjectSomeValuesFrom(ObjectInverseOf(:r) :A))
         SubClassOf(:B ObjectSomeValuesFrom(ObjectInverseOf(:r) :C))
         EquivalentClasses(:D ObjectSomeValuesFrom(ObjectInverseOf(:r) ObjectIntersectionOf(:A :C)))",
        &[("B", "D")],
        &[],
    );
}

#[test]
fn r6_with_a_qualified_filler_and_the_link_back() {
    // an A's r-successor has one r⁻-successor in B, which the A is, so A ⊑ C, and the
    // A is then an s⁻-predecessor of the successor, s being under r⁻
    check(
        "SubClassOf(:A :B)
         SubObjectPropertyOf(:s ObjectInverseOf(:r))
         SubClassOf(:A ObjectSomeValuesFrom(:r :N))
         SubClassOf(:N ObjectMaxCardinality(1 ObjectInverseOf(:r) :B))
         SubClassOf(:N ObjectSomeValuesFrom(:s ObjectIntersectionOf(:B :C)))
         SubClassOf(ObjectSomeValuesFrom(ObjectInverseOf(:s) :N) :G)",
        &[("A", "C"), ("A", "G")],
        &[("B", "C"), ("B", "G")],
    );
}

#[test]
fn self_restrictions_count_as_successors() {
    // hand-self-cardinality
    check(
        "SubClassOf(:A ObjectIntersectionOf(ObjectHasSelf(:r) ObjectMaxCardinality(1 :r owl:Thing)))
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:A :C)
         EquivalentClasses(:D ObjectIntersectionOf(:B :C))",
        &[("A", "D")],
        &[],
    );
}

#[test]
fn irreflexive_asymmetric_and_disjoint_roles() {
    // hand-asymmetric-self
    unsat(
        "AsymmetricObjectProperty(:r)
         EquivalentClasses(:A ObjectHasSelf(:r))",
        &["A"],
        &[],
    );
    unsat(
        "IrreflexiveObjectProperty(:r)
         SubClassOf(:A ObjectHasSelf(:r))
         SubClassOf(:E ObjectSomeValuesFrom(:r :E))",
        &["A"],
        &["E"],
    );
    // two successors that a functional superrole merges, by disjoint roles
    unsat(
        "FunctionalObjectProperty(:t)
         SubObjectPropertyOf(:r :t)
         SubObjectPropertyOf(:s :t)
         DisjointObjectProperties(:r :s)
         SubClassOf(:A ObjectIntersectionOf(ObjectSomeValuesFrom(:r :B) ObjectSomeValuesFrom(:s :C)))
         SubClassOf(:E ObjectSomeValuesFrom(:r :B))",
        &["A"],
        &["E"],
    );
    // a symmetric subrole of an asymmetric role relates nothing
    unsat(
        "AsymmetricObjectProperty(:r)
         SymmetricObjectProperty(:s)
         SubObjectPropertyOf(:s :r)
         SubClassOf(:A ObjectSomeValuesFrom(:s :B))
         SubClassOf(:E ObjectSomeValuesFrom(:r :B))",
        &["A"],
        &["E"],
    );
    // the predecessor that an at-most restriction identifies with a successor is
    // linked back by the successor's role, which an asymmetric role forbids
    unsat(
        "AsymmetricObjectProperty(:r)
         SubObjectPropertyOf(:r :t)
         SubObjectPropertyOf(ObjectInverseOf(:r) :t)
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:B ObjectMaxCardinality(1 :t owl:Thing))
         SubClassOf(:B ObjectSomeValuesFrom(:r :C))
         SubClassOf(:E ObjectSomeValuesFrom(:r :F))",
        &["A"],
        &["B", "E"],
    );
}

#[test]
fn negations_move_to_their_side() {
    check(
        "DisjointClasses(ObjectComplementOf(:A) :B)
         SubClassOf(:C ObjectComplementOf(:E))
         SubClassOf(:F ObjectIntersectionOf(:C :E))
         SubClassOf(ObjectUnionOf(:G :H) :I)
         SubClassOf(:J :H)
         SubClassOf(:K ObjectAllValuesFrom(:r ObjectComplementOf(:L)))
         SubClassOf(:K ObjectSomeValuesFrom(:r :L))",
        &[("B", "A"), ("J", "I"), ("G", "I")],
        &[("A", "B")],
    );
    unsat(
        "SubClassOf(:C ObjectComplementOf(:E)) SubClassOf(:F ObjectIntersectionOf(:C :E)) SubClassOf(:K ObjectAllValuesFrom(:r ObjectComplementOf(:L))) SubClassOf(:K ObjectSomeValuesFrom(:r :L))",
        &["F", "K"],
        &["C", "E", "L"],
    );
}

#[test]
fn axioms_the_horn_rules_cannot_take_stay_left_out() {
    // a data property keeps Tier D out, so the Horn rules take what they can
    let (out, ex) = run("SubClassOf(:A ObjectAllValuesFrom(:r :B))
         SubClassOf(:A ObjectUnionOf(:B :C))
         SubClassOf(ObjectAllValuesFrom(:r :B) :D)
         SubClassOf(:A ObjectMaxCardinality(2 :r :B))
         SubClassOf(:E DataSomeValuesFrom(:d owl:real))");
    assert_eq!(ex.len(), 3, "{ex:#?}");
    assert_eq!(out.stats.outside_el.taken, 1);
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    // without it Tier D takes the union and the universals, and only the counting stays
    // out
    let (out, ex) = run("SubClassOf(:A ObjectAllValuesFrom(:r :B))
         SubClassOf(:A ObjectUnionOf(:B :C))
         SubClassOf(ObjectAllValuesFrom(:r :B) :D)
         SubClassOf(:A ObjectMaxCardinality(2 :r :B))");
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert_eq!(out.stats.outside_el.taken, 3);
    // with an individual the run keeps the EL rules alone
    let (out, ex) = run("SubClassOf(:A ObjectAllValuesFrom(:r :B))
         ClassAssertion(:A :a)");
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert_eq!(out.stats.outside_el.taken, 0);
}

#[test]
fn an_incremental_change_after_a_horn_run_classifies_again() {
    let ax = axioms(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:B ObjectAllValuesFrom(ObjectInverseOf(:r) :C))",
    );
    let mut k = Classifier::of(&ax);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.taxonomy.is_subclass(&c("A"), &c("C")));
    let more = axioms("SubClassOf(:E :A)");
    k.add(&more);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.taxonomy.is_subclass(&c("E"), &c("C")));
    k.remove(&ax[1..]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.taxonomy.is_subclass(&c("E"), &c("C")));
    assert!(k.excluded().is_empty());
    // an EL run that a Horn axiom joins starts its contexts again
    let mut k = Classifier::of(&axioms("SubClassOf(:A ObjectSomeValuesFrom(:r :B))"));
    k.classify(&Limits::default()).unwrap();
    k.add(&axioms(
        "SubClassOf(:B ObjectAllValuesFrom(ObjectInverseOf(:r) :C))",
    ));
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.taxonomy.is_subclass(&c("A"), &c("C")));
}

/// A generated OWL 2 QL ontology: existential restrictions on roles and inverses on the
/// right, domains and ranges of both, inclusions between roles and inverses, and
/// disjoint classes.
fn ql(n: usize) -> String {
    let k = n / 10 + 1;
    let mut s = String::new();
    for i in 0..n {
        let r = i % k;
        s += &format!(
            "SubClassOf(:A{i} ObjectSomeValuesFrom(:r{r} :A{}))\n",
            (i * 7 + 1) % n
        );
        s += &format!(
            "SubClassOf(:A{i} ObjectSomeValuesFrom(ObjectInverseOf(:r{r}) :A{}))\n",
            (i * 3 + 2) % n
        );
        s += &format!("DisjointClasses(:A{i} :A{})\n", (i + 5) % n);
    }
    for j in 0..k {
        s += &format!("ObjectPropertyDomain(:r{j} :A{j})\n");
        s += &format!("ObjectPropertyRange(:r{j} :A{})\n", j + 1);
        s += &format!("SubClassOf(ObjectSomeValuesFrom(ObjectInverseOf(:r{j}) owl:Thing) :B{j})\n");
        s += &format!(
            "SubObjectPropertyOf(:r{j} ObjectInverseOf(:r{}))\n",
            (j + 1) % k
        );
    }
    s
}

/// A generated OWL 2 RL ontology: universals and at-most restrictions on the right,
/// existentials, unions and intersections on the left, inverses and transitive roles.
fn rl(n: usize) -> String {
    let k = n / 10 + 1;
    let mut s = String::new();
    for i in 0..n {
        let r = i % k;
        let (a, b, c) = ((i + 1) % n, (i * 7 + 3) % n, (i * 11 + 5) % n);
        s += &format!("SubClassOf(:A{i} ObjectAllValuesFrom(:r{r} :A{a}))\n");
        s += &format!("SubClassOf(:A{i} ObjectMaxCardinality(1 ObjectInverseOf(:r{r}) :A{b}))\n");
        s += &format!("SubClassOf(ObjectSomeValuesFrom(:r{r} :A{b}) :A{c})\n");
        s += &format!("SubClassOf(ObjectUnionOf(:A{a} ObjectIntersectionOf(:A{b} :A{c})) :B{i})\n");
    }
    for j in 0..k {
        s += &format!("TransitiveObjectProperty(:t{j})\n");
        s += &format!("SubObjectPropertyOf(:r{j} :t{j})\n");
        s += &format!("SubClassOf(:B{j} ObjectAllValuesFrom(ObjectInverseOf(:t{j}) :A{j}))\n");
        s += &format!("InverseObjectProperties(:r{j} :s{j})\n");
    }
    s
}

#[test]
fn ql_and_rl_ontologies_create_no_conjunction_contexts() {
    // Without positive universals or at-most restrictions, which OWL 2 QL lacks, and
    // without positive existentials, which OWL 2 RL lacks without individuals, no rule
    // creates a context, so the contexts are those of the classes and the existential
    // restrictions and grow linearly.
    for (name, make) in [("ql", ql as fn(usize) -> String), ("rl", rl)] {
        let mut sizes = Vec::new();
        for n in [100, 200, 400] {
            let (out, ex) = run(&make(n));
            assert!(ex.is_empty(), "{name} {n}: {ex:#?}");
            assert_eq!(out.stats.conjunctions, 0, "{name} {n}");
            sizes.push(out.stats.contexts);
        }
        // at most four contexts for each class of the ontology
        for (n, c) in [100, 200, 400].iter().zip(&sizes) {
            assert!(*c <= 4 * n + 4 * (n / 10 + 1), "{name}: {sizes:?}");
        }
    }
}

#[test]
fn r6_runs_when_a_predecessor_that_holds_the_filler_links_again() {
    // M is told E only after it tells N it holds B, through a second link to N by r
    check(
        "SubClassOf(:M :B)
         SubClassOf(:M ObjectSomeValuesFrom(:q :N))
         SubClassOf(:M ObjectSomeValuesFrom(:p :P))
         SubClassOf(:P ObjectAllValuesFrom(ObjectInverseOf(:p) :E))
         SubClassOf(:E ObjectSomeValuesFrom(:r :N))
         SubClassOf(:N ObjectMaxCardinality(1 ObjectInverseOf(:r) :B))
         SubClassOf(:N ObjectSomeValuesFrom(ObjectInverseOf(:r) ObjectIntersectionOf(:B :C)))",
        &[("M", "C")],
        &[("N", "C"), ("E", "C")],
    );
}

#[test]
fn r6_forces_a_loop_on_a_predecessor_in_the_same_context() {
    // an A's r-successor is its own s-successor, and the A an s-successor too, so the A
    // has an r-loop
    unsat(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :A))
         SubObjectPropertyOf(ObjectInverseOf(:r) :s)
         SubClassOf(:A ObjectHasSelf(:s))
         SubClassOf(:A ObjectMaxCardinality(1 :s))
         IrreflexiveObjectProperty(:r)
         SubClassOf(:B ObjectSomeValuesFrom(:s :B))",
        &["A"],
        &["B"],
    );
}

#[test]
fn disjoint_roles_are_disjoint_on_their_inverses() {
    // the r⁻- and s⁻-successors of an A are one, which r and s relate both
    unsat(
        "FunctionalObjectProperty(:t)
         SubObjectPropertyOf(ObjectInverseOf(:r) :t)
         SubObjectPropertyOf(ObjectInverseOf(:s) :t)
         DisjointObjectProperties(:r :s)
         SubClassOf(:A ObjectSomeValuesFrom(ObjectInverseOf(:r) :B))
         SubClassOf(:A ObjectSomeValuesFrom(ObjectInverseOf(:s) :C))
         SubClassOf(:D ObjectSomeValuesFrom(ObjectInverseOf(:r) :B))",
        &["A"],
        &["D", "B", "C"],
    );
}

#[test]
fn a_maximum_whose_dual_overflows_is_left_out() {
    // a data property keeps Tier D out, so the Horn normalization meets the maximum
    let (out, ex) = run("SubClassOf(ObjectMaxCardinality(4294967295 :r :B) :A)
         SubClassOf(:C :A)
         SubClassOf(:E DataSomeValuesFrom(:d owl:real))");
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert!(!out.taxonomy.is_subclass(&c("B"), &c("A")));
    // Tier D reads it as ⊤ ⊑ A ⊔ ∃r.B, which no count changes without counting elsewhere
    let (out, ex) = run("SubClassOf(ObjectMaxCardinality(4294967295 :r :B) :A)
         SubClassOf(:C :A)");
    assert!(ex.is_empty(), "{ex:#?}");
    assert!(!out.taxonomy.is_subclass(&c("B"), &c("A")));
}

#[test]
fn a_universal_on_a_role_with_an_automaton_holds_for_direct_successors() {
    // the automaton of r takes r itself as well as p∘q
    unsat(
        "SubObjectPropertyOf(ObjectPropertyChain(:p :q) :r)
         SubClassOf(:A ObjectAllValuesFrom(:r :C))
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         DisjointClasses(:B :C)
         SubClassOf(:D ObjectSomeValuesFrom(:p ObjectSomeValuesFrom(:q :E)))
         SubClassOf(:D ObjectAllValuesFrom(:r :F))
         SubClassOf(ObjectSomeValuesFrom(:p ObjectSomeValuesFrom(:q ObjectIntersectionOf(:E :F))) :G)",
        &["A"],
        &["D", "B", "C"],
    );
    check(
        "SubObjectPropertyOf(ObjectPropertyChain(:p :q) :r)
         SubClassOf(:D ObjectSomeValuesFrom(:p ObjectSomeValuesFrom(:q :E)))
         SubClassOf(:D ObjectAllValuesFrom(:r :F))
         SubClassOf(ObjectSomeValuesFrom(:p ObjectSomeValuesFrom(:q ObjectIntersectionOf(:E :F))) :G)",
        &[("D", "G")],
        &[("E", "F")],
    );
}

#[test]
fn an_incremental_change_after_a_horn_normalization_without_horn_rules_classifies_again() {
    // ≥2 r.C becomes two existentials, which the EL rules take, but the EL invalidation
    // cannot read the at-least restriction
    let base = axioms(
        "SubClassOf(:A :B)
         SubClassOf(ObjectSomeValuesFrom(:r :C) :D)",
    );
    let more = axioms("SubClassOf(:A ObjectMinCardinality(2 :r :C))");
    let mut k = Classifier::of(&base);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
    k.add(&more);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    k.remove(&more);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
}

#[test]
fn the_el_only_mode_gives_the_el_answer_and_the_default_the_horn_one() {
    // GO's pattern: part of is transitive and the inverse of has part, and a
    // membrane-bounded organelle is an organelle with a membrane part
    let body = "TransitiveObjectProperty(:part_of)
         InverseObjectProperties(:part_of :has_part)
         SubClassOf(:FilopodiumMembrane :Membrane)
         SubClassOf(:FilopodiumMembrane ObjectSomeValuesFrom(:part_of :Filopodium))
         SubClassOf(:Filopodium ObjectSomeValuesFrom(:part_of :Cytoskeleton))
         SubClassOf(:Cytoskeleton :Organelle)
         EquivalentClasses(:MembraneBounded ObjectIntersectionOf(:Organelle ObjectSomeValuesFrom(:has_part :Membrane)))
         EquivalentClasses(:OrganelleMembrane ObjectIntersectionOf(:Membrane ObjectSomeValuesFrom(:part_of :MembraneBounded)))";
    let ax = axioms(body);
    let mut answers = Vec::new();
    for el_only in [true, false] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                el_only,
                ..Limits::default()
            })
            .unwrap();
        answers.push((out, k.excluded()));
    }
    let (el, el_ex) = &answers[0];
    let (horn, horn_ex) = &answers[1];
    // the ELK mode leaves the inverse out, as ELK does, and misses the subsumption
    assert_eq!(el_ex.len(), 1, "{el_ex:#?}");
    assert!(
        !el.taxonomy
            .is_subclass(&c("FilopodiumMembrane"), &c("OrganelleMembrane"))
    );
    assert_eq!(el.stats.conjunctions, 0);
    // the default takes it with the Horn rules
    assert!(horn_ex.is_empty(), "{horn_ex:#?}");
    assert!(
        horn.taxonomy
            .is_subclass(&c("FilopodiumMembrane"), &c("OrganelleMembrane"))
    );
    // only the cytoskeleton the membrane is part of has it as a part
    assert!(
        !horn
            .taxonomy
            .is_subclass(&c("Cytoskeleton"), &c("MembraneBounded"))
    );
    // the ELK mode is the EL classification of the ontology without the inverse
    let el_part: Vec<Axiom> = ax
        .iter()
        .filter(|a| !matches!(a.kind, AxiomKind::InverseObjectProperties(..)))
        .cloned()
        .collect();
    let mut k = Classifier::of(&el_part);
    let plain = k.classify(&Limits::default()).unwrap();
    assert!(k.excluded().is_empty());
    let pairs = |o: &Classification| -> Vec<(String, String)> {
        let t = &o.taxonomy;
        let cs: Vec<Class> = t.classes().cloned().collect();
        let mut v = Vec::new();
        for x in &cs {
            for y in &cs {
                if t.is_subclass(x, y) {
                    v.push((x.as_str().to_string(), y.as_str().to_string()));
                }
            }
        }
        v.sort();
        v
    };
    assert_eq!(pairs(el), pairs(&plain));
    // a kept classifier that changes mode classifies again
    let mut k = Classifier::of(&ax);
    let elk = Limits {
        el_only: true,
        ..Limits::default()
    };
    k.classify(&elk).unwrap();
    let out = k.classify(&Limits::default()).unwrap();
    assert!(
        out.taxonomy
            .is_subclass(&c("FilopodiumMembrane"), &c("OrganelleMembrane"))
    );
    let out = k.classify(&elk).unwrap();
    assert!(
        !out.taxonomy
            .is_subclass(&c("FilopodiumMembrane"), &c("OrganelleMembrane"))
    );
}

#[test]
fn a_run_whose_horn_rules_pass_the_budget_classifies_by_the_el_rules() {
    // GO's pattern, whose Horn run needs more memory than its EL run
    let body = "TransitiveObjectProperty(:part_of)
         InverseObjectProperties(:part_of :has_part)
         SubClassOf(:FilopodiumMembrane :Membrane)
         SubClassOf(:FilopodiumMembrane ObjectSomeValuesFrom(:part_of :Filopodium))
         SubClassOf(:Filopodium ObjectSomeValuesFrom(:part_of :Cytoskeleton))
         SubClassOf(:Cytoskeleton :Organelle)
         EquivalentClasses(:MembraneBounded ObjectIntersectionOf(:Organelle ObjectSomeValuesFrom(:has_part :Membrane)))
         EquivalentClasses(:OrganelleMembrane ObjectIntersectionOf(:Membrane ObjectSomeValuesFrom(:part_of :MembraneBounded)))";
    let ax = axioms(body);
    let limits = |el_only: bool, max_memory: Option<usize>| Limits {
        threads: 1,
        el_only,
        max_memory,
        ..Limits::default()
    };
    let mut k = Classifier::of(&ax);
    let el = k.classify(&limits(true, None)).unwrap();
    let el_ex = k.excluded();
    let mut k = Classifier::of(&ax);
    let horn = k.classify(&limits(false, None)).unwrap();
    assert!(!horn.stats.horn_over_budget);
    assert!(
        horn.stats.peak_memory > el.stats.peak_memory,
        "{} {}",
        horn.stats.peak_memory,
        el.stats.peak_memory
    );
    // a budget the EL run fits in and the Horn run does not
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&limits(false, Some(el.stats.peak_memory)))
        .unwrap();
    assert!(out.stats.horn_over_budget);
    assert_eq!(out.stats.conjunctions, 0);
    assert!(out.stats.peak_memory > el.stats.peak_memory);
    assert_eq!(k.excluded(), el_ex);
    assert!(
        !out.taxonomy
            .is_subclass(&c("FilopodiumMembrane"), &c("OrganelleMembrane"))
    );
    // the classifier is whole again: a run without the budget takes the Horn rules
    let out = k.classify(&limits(false, None)).unwrap();
    assert!(!out.stats.horn_over_budget);
    assert!(
        out.taxonomy
            .is_subclass(&c("FilopodiumMembrane"), &c("OrganelleMembrane"))
    );
    // a budget the EL run does not fit in either is an error
    let mut k = Classifier::of(&ax);
    assert!(
        k.classify(&limits(false, Some(el.stats.peak_memory / 2)))
            .is_err()
    );
}

#[test]
fn the_el_run_after_the_horn_budget_draws_on_what_the_pool_admitted() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let body = "TransitiveObjectProperty(:part_of)
         InverseObjectProperties(:part_of :has_part)
         SubClassOf(:FilopodiumMembrane :Membrane)
         SubClassOf(:FilopodiumMembrane ObjectSomeValuesFrom(:part_of :Filopodium))
         SubClassOf(:Filopodium ObjectSomeValuesFrom(:part_of :Cytoskeleton))
         SubClassOf(:Cytoskeleton :Organelle)
         EquivalentClasses(:MembraneBounded ObjectIntersectionOf(:Organelle ObjectSomeValuesFrom(:has_part :Membrane)))
         EquivalentClasses(:OrganelleMembrane ObjectIntersectionOf(:Membrane ObjectSomeValuesFrom(:part_of :MembraneBounded)))";
    let ax = axioms(body);
    // a pool that admits charges up to `cap` and keeps them, as a server's grant does
    let pool = |cap: usize| {
        let held = Arc::new(AtomicUsize::new(0));
        let h = held.clone();
        let charge: Arc<dyn Fn(usize) -> bool + Send + Sync> = Arc::new(move |bytes: usize| {
            h.try_update(Ordering::Relaxed, Ordering::Relaxed, |x| {
                (x + bytes <= cap).then_some(x + bytes)
            })
            .is_ok()
        });
        (held, charge)
    };
    let limits = |el_only: bool, charge| Limits {
        threads: 1,
        el_only,
        charge: Some(charge),
        ..Limits::default()
    };
    let (el_held, charge) = pool(usize::MAX);
    Classifier::of(&ax).classify(&limits(true, charge)).unwrap();
    let el_need = el_held.load(Ordering::Relaxed);
    let (horn_held, charge) = pool(usize::MAX);
    let horn = Classifier::of(&ax)
        .classify(&limits(false, charge))
        .unwrap();
    assert!(!horn.stats.horn_over_budget);
    assert!(horn_held.load(Ordering::Relaxed) > el_need);
    // a pool the EL run fits in: the Horn run passes it, and the EL run charges only
    // what it needs beyond what the pool admitted for the Horn run
    let (held, charge) = pool(el_need);
    let out = Classifier::of(&ax)
        .classify(&limits(false, charge))
        .unwrap();
    assert!(out.stats.horn_over_budget);
    assert!(held.load(Ordering::Relaxed) <= el_need);
    assert!(
        !out.taxonomy
            .is_subclass(&c("FilopodiumMembrane"), &c("OrganelleMembrane"))
    );
}

/// A universal on a role no chain reaches, which puts an ontology on the Horn rules, so
/// that a run carries its chains by the automata alone and composes none (item 693).
const HORN: &str = "SubClassOf(:X ObjectAllValuesFrom(:u :Y))";

/// Checks the body on the Horn rules, with no chain composed.
fn chain_free(body: &str, yes: &[(&str, &str)], no: &[(&str, &str)]) {
    let out = check(&format!("{HORN}\n{body}"), yes, no);
    assert_eq!(out.stats.tier, Tier::Horn);
}

#[test]
fn a_chain_reaches_a_negative_existential_without_composed_links() {
    chain_free(
        "SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s :B)))
         EquivalentClasses(:C ObjectSomeValuesFrom(:t :B))",
        &[("A", "C")],
        &[("B", "C")],
    );
}

#[test]
fn a_chain_through_an_inverse_reaches_a_negative_existential() {
    chain_free(
        "SubObjectPropertyOf(ObjectPropertyChain(ObjectInverseOf(:r) :s) :t)
         SubClassOf(:A ObjectSomeValuesFrom(ObjectInverseOf(:r) ObjectSomeValuesFrom(:s :B)))
         EquivalentClasses(:C ObjectSomeValuesFrom(:t :B))",
        &[("A", "C")],
        &[],
    );
    // the chain's inverse, read from the end of the path back to its start
    chain_free(
        "SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubClassOf(:B ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectSomeValuesFrom(ObjectInverseOf(:r) :A)))
         EquivalentClasses(:C ObjectSomeValuesFrom(ObjectInverseOf(:t) :A))",
        &[("B", "C")],
        &[("A", "C")],
    );
}

#[test]
fn a_transitive_role_reaches_a_negative_existential() {
    chain_free(
        "TransitiveObjectProperty(:t)
         SubClassOf(:A ObjectSomeValuesFrom(:t ObjectSomeValuesFrom(:t ObjectSomeValuesFrom(:t :B))))
         EquivalentClasses(:C ObjectSomeValuesFrom(:t :B))
         SubObjectPropertyOf(:t :w)
         EquivalentClasses(:D ObjectSomeValuesFrom(:w :B))",
        &[("A", "C"), ("A", "D")],
        &[("B", "C")],
    );
    // part of and has part, inverse to each other, as GO and Uberon have them
    chain_free(
        "TransitiveObjectProperty(:part_of)
         InverseObjectProperties(:part_of :has_part)
         SubClassOf(:A ObjectSomeValuesFrom(:has_part ObjectSomeValuesFrom(:has_part :B)))
         EquivalentClasses(:C ObjectSomeValuesFrom(:has_part :B))
         SubClassOf(:B2 ObjectSomeValuesFrom(:part_of ObjectSomeValuesFrom(:part_of :A2)))
         EquivalentClasses(:C2 ObjectSomeValuesFrom(ObjectInverseOf(:has_part) :A2))",
        &[("A", "C"), ("B2", "C2")],
        &[],
    );
}

#[test]
fn a_chain_whose_middle_class_carries_a_universal() {
    chain_free(
        "SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubClassOf(:A ObjectSomeValuesFrom(:r :M))
         SubClassOf(:M ObjectSomeValuesFrom(:s :B))
         SubClassOf(:M ObjectAllValuesFrom(:s :D))
         EquivalentClasses(:C ObjectSomeValuesFrom(:t ObjectIntersectionOf(:B :D)))",
        &[("A", "C")],
        &[("M", "C"), ("B", "D")],
    );
}

#[test]
fn a_conjunction_context_reached_only_through_a_chain() {
    // A's universal on t meets B two steps on, in a context that holds B and D, whose
    // subsumer E returns to A along the inverse of the chain
    chain_free(
        "SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubClassOf(:A ObjectAllValuesFrom(:t :D))
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s :B)))
         SubClassOf(ObjectIntersectionOf(:B :D) :E)
         EquivalentClasses(:G ObjectSomeValuesFrom(:t :E))",
        &[("A", "G")],
        &[("B", "E"), ("B", "G")],
    );
}

#[test]
fn domains_and_ranges_hold_along_a_chain() {
    chain_free(
        "SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         ObjectPropertyDomain(:t :C)
         ObjectPropertyRange(:t :D)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s :B)))
         EquivalentClasses(:E ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s ObjectIntersectionOf(:B :D))))",
        &[("A", "C"), ("A", "E")],
        &[("B", "D")],
    );
}

#[test]
fn nested_chains_and_superroles_reach_a_negative_existential() {
    chain_free(
        "SubObjectPropertyOf(ObjectPropertyChain(:p :q) :r)
         SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubObjectPropertyOf(:t :w)
         SubClassOf(:A ObjectSomeValuesFrom(:p ObjectSomeValuesFrom(:q ObjectSomeValuesFrom(:s :B))))
         EquivalentClasses(:C ObjectSomeValuesFrom(:w :B))",
        &[("A", "C")],
        &[],
    );
}

#[test]
fn a_reflexive_operand_of_a_chain() {
    chain_free(
        "ReflexiveObjectProperty(:r)
         SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubClassOf(:A ObjectSomeValuesFrom(:s :B))
         EquivalentClasses(:C ObjectSomeValuesFrom(:t :B))",
        &[("A", "C")],
        &[("B", "C")],
    );
}

#[test]
fn counting_on_a_simple_role_beside_a_chain() {
    // the two f-successors of A are one, two steps under t from Z
    chain_free(
        "FunctionalObjectProperty(:f)
         SubObjectPropertyOf(ObjectPropertyChain(:r :f) :t)
         SubClassOf(:Z ObjectSomeValuesFrom(:r :A))
         SubClassOf(:A ObjectIntersectionOf(ObjectSomeValuesFrom(:f :B) ObjectSomeValuesFrom(:f :C)))
         EquivalentClasses(:D ObjectSomeValuesFrom(:t ObjectIntersectionOf(:B :C)))",
        &[("Z", "D")],
        &[("A", "D")],
    );
}

#[test]
fn an_empty_role_keeps_the_composed_chains() {
    // with a role under the bottom property, the run composes its chains as for EL
    chain_free(
        "SubObjectPropertyOf(:e owl:bottomObjectProperty)
         SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s :B)))
         EquivalentClasses(:C ObjectSomeValuesFrom(:t :B))
         SubClassOf(:F ObjectSomeValuesFrom(:e :B))",
        &[("A", "C")],
        &[("B", "C")],
    );
}

#[test]
fn a_chain_over_an_inverse_holds_in_its_mirror_image() {
    // r0 ∘ r2⁻ ⊑ r0 with r2 the inverse of r0 makes r0 transitive, and so r2
    let body = "SubObjectPropertyOf(ObjectPropertyChain(:r0 ObjectInverseOf(:r2)) :r0)
         InverseObjectProperties(:r2 :r0)
         SubClassOf(:A5 ObjectSomeValuesFrom(:r2 :A0))
         EquivalentClasses(:A0 ObjectSomeValuesFrom(:r2 ObjectIntersectionOf(:A3 :A2)))";
    check(body, &[("A5", "A0")], &[("A0", "A5")]);
    chain_free(body, &[("A5", "A0")], &[("A0", "A5")]);
    // with an empty role, which keeps every chain composed
    check(
        &format!(
            "{body}\nSubObjectPropertyOf(:e owl:bottomObjectProperty)\nSubClassOf(:F ObjectSomeValuesFrom(:e :A3))"
        ),
        &[("A5", "A0")],
        &[("A0", "A5")],
    );
}

#[test]
fn r5_merges_every_counted_successor_at_once() {
    // A1's at-most restriction counts successors that arrive one by one; merging them in
    // pairs made a context for each subset on the way and passed the Horn budget
    let (out, ex) = run("SubObjectPropertyOf(:r1 :r0)
         InverseObjectProperties(:r0 :r1)
         EquivalentClasses(:A0 ObjectSomeValuesFrom(:r0 ObjectIntersectionOf(:A5 :A4)))
         EquivalentClasses(:A6 ObjectSomeValuesFrom(:r1 ObjectIntersectionOf(:A2 :A3)))
         SubClassOf(:A3 ObjectAllValuesFrom(:r0 :A1))
         SubClassOf(:A7 :A0)
         EquivalentClasses(:A7 ObjectSomeValuesFrom(:r0 ObjectIntersectionOf(:A2 :A1)))
         SubClassOf(:A6 ObjectSomeValuesFrom(:r1 ObjectSomeValuesFrom(:r1 :A5)))
         SubClassOf(:A1 ObjectMaxCardinality(1 :r1 :A3))
         SubClassOf(:A0 ObjectSomeValuesFrom(ObjectInverseOf(:r0) ObjectSomeValuesFrom(:r1 :A6)))
         SubClassOf(ObjectSomeValuesFrom(ObjectInverseOf(:r1) :A5) :A3)
         SubClassOf(ObjectSomeValuesFrom(:r0 :A5) :A6)");
    assert!(ex.is_empty(), "{ex:#?}");
    assert!(!out.stats.horn_over_budget);
    assert!(out.stats.conjunctions < 1_000, "{}", out.stats.conjunctions);
    assert!(out.taxonomy.is_subclass(&c("A0"), &c("A6")));
}

#[test]
fn a_filler_no_rule_reads_still_reaches_named_classes() {
    // C has no told subsumer and no rule takes it as a premise: R3 passes over it, but
    // R4 takes it to the predecessor, and a loop by a reflexive role to the class itself
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:B ObjectAllValuesFrom(ObjectInverseOf(:r) :C))
         ReflexiveObjectProperty(:s)
         SubClassOf(:D ObjectAllValuesFrom(:s :C))
         SubClassOf(:E ObjectSomeValuesFrom(:q :F))
         SubClassOf(:E ObjectAllValuesFrom(:q :C))
         EquivalentClasses(:G ObjectSomeValuesFrom(:q ObjectIntersectionOf(:F :C)))",
        &[("A", "C"), ("D", "C"), ("E", "G")],
        &[("B", "C"), ("F", "C")],
    );
}

#[test]
fn mirror_images_of_longer_chains_told_mirrors_and_superroles() {
    // three operands: q ∘ s⁻ ∘ r⁻ ⊑ t⁻ is the mirror image of r ∘ s ∘ q⁻ ⊑ t
    let three = "SubObjectPropertyOf(ObjectPropertyChain(:r :s ObjectInverseOf(:q)) :t)
         SubClassOf(:A ObjectSomeValuesFrom(:q ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectSomeValuesFrom(ObjectInverseOf(:r) :B))))
         EquivalentClasses(:C ObjectSomeValuesFrom(ObjectInverseOf(:t) :B))";
    // a chain told beside its own mirror image, which is composed once
    let told = "SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
         SubObjectPropertyOf(ObjectPropertyChain(ObjectInverseOf(:s) ObjectInverseOf(:r)) ObjectInverseOf(:t))
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s :B)))
         SubClassOf(:D ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectSomeValuesFrom(ObjectInverseOf(:r) :E)))
         EquivalentClasses(:C ObjectSomeValuesFrom(:t :B))
         EquivalentClasses(:F ObjectSomeValuesFrom(ObjectInverseOf(:t) :E))";
    // the mirror image's result under the inverse of a superrole
    let sup = "SubObjectPropertyOf(ObjectPropertyChain(:r ObjectInverseOf(:s)) :t)
         SubObjectPropertyOf(:t :u)
         SubClassOf(:A ObjectSomeValuesFrom(:s ObjectSomeValuesFrom(ObjectInverseOf(:r) :B)))
         EquivalentClasses(:C ObjectSomeValuesFrom(ObjectInverseOf(:u) :B))";
    for body in [three, sup] {
        check(body, &[("A", "C")], &[("B", "C")]);
        chain_free(body, &[("A", "C")], &[("B", "C")]);
    }
    check(told, &[("A", "C"), ("D", "F")], &[("A", "F"), ("D", "C")]);
    chain_free(told, &[("A", "C"), ("D", "F")], &[("A", "F"), ("D", "C")]);
}

#[test]
fn r5_over_successors_that_hold_only_the_top_class() {
    // both successors' cores are empty, so their merge changes nothing
    check(
        "SubObjectPropertyOf(:r :s)
         SubObjectPropertyOf(:q :s)
         FunctionalObjectProperty(:s)
         SubClassOf(:A ObjectIntersectionOf(ObjectSomeValuesFrom(:r owl:Thing) ObjectSomeValuesFrom(:q owl:Thing)))
         SubClassOf(ObjectSomeValuesFrom(:s owl:Thing) :B)",
        &[("A", "B")],
        &[("B", "A")],
    );
}

#[test]
fn a_cancellation_polled_once_stops_a_horn_run_wherever_it_comes() {
    // 40,000 classes no rule reads under one superclass: enough work for the absorbed
    // fillers' pass to poll, with a saturation that stays linear
    let mut body = String::from("SubClassOf(:X ObjectAllValuesFrom(:u :Y))\n");
    for i in 0..40_000 {
        body.push_str(&format!("SubClassOf(:C{i} :D)\n"));
    }
    let ax = axioms(&body);
    let counting = |stop_at: usize| {
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let m = n.clone();
        let limits = Limits {
            threads: 1,
            cancelled: Some(std::sync::Arc::new(move || {
                m.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1 == stop_at
            })),
            ..Limits::default()
        };
        (n, limits)
    };
    let (n, limits) = counting(0);
    let out = Classifier::of(&ax).classify(&limits).unwrap();
    assert_eq!(out.stats.tier, Tier::Horn);
    let polls = n.load(std::sync::atomic::Ordering::Relaxed);
    for k in 1..=polls {
        let (_, limits) = counting(k);
        let r = Classifier::of(&ax).classify(&limits);
        assert!(
            matches!(r, Err(sparkles_owl::classify::Error::Cancelled)),
            "poll {k} of {polls}"
        );
    }
}
