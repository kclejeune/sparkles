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
    let (out, ex) = run("SubClassOf(:A ObjectAllValuesFrom(:r :B))
         SubClassOf(:A ObjectUnionOf(:B :C))
         SubClassOf(ObjectAllValuesFrom(:r :B) :D)
         SubClassOf(:A ObjectMaxCardinality(2 :r :B))");
    assert_eq!(ex.len(), 3, "{ex:#?}");
    assert_eq!(out.stats.outside_el.taken, 1);
    assert_eq!(out.stats.tier, Tier::Disjunctive);
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
    let (out, ex) = run("SubClassOf(ObjectMaxCardinality(4294967295 :r :B) :A)
         SubClassOf(:C :A)");
    assert_eq!(ex.len(), 1, "{ex:#?}");
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
