use super::*;

/// The axioms of a functional-style body over the prefix `:`, and those of them that the
/// lines marked `#out` hold, which stand for the axioms a run left out.
fn read(body: &str) -> (Vec<AxiomKind>, Vec<AxiomKind>) {
    let mut kept = String::new();
    let mut out = String::new();
    for line in body.lines() {
        match line.trim().strip_suffix("#out") {
            Some(l) => {
                kept.push_str(l);
                kept.push('\n');
                out.push_str(l);
                out.push('\n');
            }
            None => {
                kept.push_str(line);
                kept.push('\n');
            }
        }
    }
    let doc = |b: &str| {
        crate::ofn_read::parse(&format!(
            "Prefix(:=<http://e/#>)\nOntology(<http://e/o>\n{b}\n)"
        ))
        .unwrap()
        .axioms
        .into_iter()
        .map(|a| a.kind)
        .collect::<Vec<_>>()
    };
    (doc(&kept), doc(&out))
}

fn cov(body: &str, classes: &[&str]) -> Coverage {
    let (all, out) = read(body);
    let all: Vec<&AxiomKind> = all.iter().collect();
    let out: Vec<&AxiomKind> = out.iter().collect();
    let classes: Vec<Class> = classes
        .iter()
        .map(|c| Class::new(format!("http://e/#{c}")))
        .collect();
    coverage(&all, &out, true, classes.iter(), usize::MAX, &|| false).unwrap()
}

fn names(c: &Coverage) -> Vec<String> {
    c.incomplete
        .iter()
        .map(|c| c.as_str().trim_start_matches("http://e/#").to_string())
        .collect()
}

const IRL1: &str = "
SubObjectPropertyOf(:r :s)
SubClassOf(:K ObjectSomeValuesFrom(:r :S))
SubClassOf(:S ObjectUnionOf(:D1 :D2)) #out
SubClassOf(:D1 ObjectAllValuesFrom(ObjectInverseOf(:s) :Ans))
SubClassOf(:D2 ObjectAllValuesFrom(ObjectInverseOf(:s) :Ans))
ClassAssertion(:K :a)
";

#[test]
fn an_assertion_that_reaches_a_left_out_axiom_leaves_the_other_classes_complete() {
    // the bottom module of the empty signature holds every axiom through K(a), the top
    // module holds none, and the terminology's modules decide the classes
    let c = cov(IRL1, &["K", "S", "D1", "D2", "Ans"]);
    assert!(c.consistency_known);
    assert_eq!(names(&c), ["K", "S"]);
}

#[test]
fn a_nominal_turns_the_terminology_layer_off() {
    let c = cov(
        "
SubClassOf(:A ObjectSomeValuesFrom(:r :C))
SubClassOf(:A ObjectAllValuesFrom(:r :D))
SubClassOf(ObjectIntersectionOf(:C :D) ObjectUnionOf(:E :F)) #out
SubClassOf(:E :G)
SubClassOf(:F :G)
SubClassOf(ObjectSomeValuesFrom(:r :G) :H)
SubClassOf(:A ObjectSomeValuesFrom(:s ObjectOneOf(:u)))
",
        &["A", "C", "D", "E", "F", "G", "H"],
    );
    assert!(c.consistency_known);
    assert_eq!(names(&c), ["A"]);
    // with a nominal reached from an assertion, the assertion's module decides
    let c = cov(
        "
SubClassOf(:C ObjectOneOf(:a))
ClassAssertion(:E :a)
SubClassOf(:E ObjectUnionOf(:D1 :D2)) #out
SubClassOf(:D1 :D)
SubClassOf(:D2 :D)
",
        &["C", "D", "D1", "D2", "E"],
    );
    assert_eq!(names(&c), ["C", "D", "D1", "D2", "E"]);
}

#[test]
fn the_universal_property_turns_the_terminology_layer_off() {
    let c = cov(
        "
ClassAssertion(:E :a)
SubClassOf(:E ObjectUnionOf(:D1 :D2)) #out
SubClassOf(:D1 :D)
SubClassOf(:D2 :D)
SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty :D) :G)
SubClassOf(:C :X)
",
        &["C", "D", "E", "G", "X"],
    );
    assert_eq!(names(&c), ["C", "D", "E", "G", "X"]);
}

#[test]
fn a_left_out_axiom_that_can_make_the_ontology_inconsistent_leaves_it_unknown() {
    let c = cov(
        "
ClassAssertion(:A :a)
SubClassOf(:A ObjectUnionOf(:B :C)) #out
SubClassOf(:B owl:Nothing)
SubClassOf(:C owl:Nothing)
SubClassOf(:X :Y)
",
        &["A", "B", "C", "X", "Y"],
    );
    assert!(!c.consistency_known);
    assert_eq!(names(&c), ["A", "B", "C", "X", "Y"]);
}

#[test]
fn an_inconsistent_run_is_complete() {
    let (all, out) = read("SubClassOf(:A ObjectUnionOf(:B :C)) #out");
    let all: Vec<&AxiomKind> = all.iter().collect();
    let out: Vec<&AxiomKind> = out.iter().collect();
    let a = Class::new("http://e/#A");
    let c = coverage(&all, &out, false, [&a], usize::MAX, &|| false).unwrap();
    assert!(c.consistency_known && c.incomplete.is_empty());
}

#[test]
fn a_left_out_axiom_that_is_not_given_leaves_everything_unknown() {
    let (all, _) = read("SubClassOf(:A :B)");
    let (_, out) = read("SubClassOf(:A ObjectUnionOf(:B :C)) #out");
    let all: Vec<&AxiomKind> = all.iter().collect();
    let out: Vec<&AxiomKind> = out.iter().collect();
    let a = Class::new("http://e/#A");
    let c = coverage(&all, &out, true, [&a], usize::MAX, &|| false).unwrap();
    assert!(!c.consistency_known);
    assert_eq!(c.incomplete, [a]);
}

#[test]
fn a_global_left_out_axiom_reaches_every_class() {
    let c = cov(
        "
SubClassOf(owl:Thing ObjectUnionOf(:B :C)) #out
SubClassOf(:X :Y)
",
        &["B", "C", "X", "Y"],
    );
    // owl:Thing ⊑ B ⊔ C is in the bottom module of the empty signature, and is
    // top-local, so the consistency is known but every class is incomplete
    assert!(c.consistency_known);
    assert_eq!(names(&c), ["B", "C", "X", "Y"]);
}

#[test]
fn a_clean_module_marks_its_classes_complete_and_a_dirty_one_its_users() {
    let c = cov(
        "
SubClassOf(:A :B)
SubClassOf(:B :C)
SubClassOf(:C ObjectUnionOf(:D :E)) #out
SubClassOf(:X :B)
SubClassOf(:P :Q)
",
        &["A", "B", "C", "D", "E", "P", "Q", "X"],
    );
    assert_eq!(names(&c), ["A", "B", "C", "X"]);
}

/// The bottom-locality rules for keys and disjoint unions, which an earlier version
/// called local whatever their signature.
#[test]
fn keys_and_disjoint_unions_of_everything_are_not_local() {
    // a key over a class in the signature can make named individuals equal
    let c = cov(
        "
HasKey(:A (:p) ()) #out
SubClassOf(:A :B)
",
        &["A", "B"],
    );
    // the key is local for the empty signature, so the consistency is known, and it
    // is on the assertion side of the terminology layer, which leaves A complete
    assert!(c.consistency_known);
    assert!(names(&c).is_empty(), "{c:?}");
    let s = |names: &[&str]| -> FxHashSet<String> {
        names.iter().map(|n| format!("http://e/#{n}")).collect()
    };
    let (key, _) = read("HasKey(:A (:p) ())");
    let sig = s(&["A", "p"]);
    let has = |iri: &str| sig.contains(iri);
    assert!(!bottom_local(&key[0], &Sig(&has)));
    let a = s(&["A"]);
    let only_a = |iri: &str| a.contains(iri);
    assert!(bottom_local(&key[0], &Sig(&only_a)));
    let (union, _) = read("DisjointUnion(owl:Thing :B :C)");
    let none = |_: &str| false;
    assert!(!bottom_local(&union[0], &Sig(&none)));
    let (union, _) = read("DisjointUnion(:A :B :C)");
    assert!(bottom_local(&union[0], &Sig(&none)));
}

#[test]
fn top_locality_keeps_what_every_pair_and_everything_break() {
    let none = |_: &str| false;
    let s = Sig(&none);
    let local = |body: &str| {
        let (k, _) = read(body);
        top_local(&k[0], &s)
    };
    assert!(local("SubClassOf(:A ObjectSomeValuesFrom(:r :B))"));
    assert!(local("ClassAssertion(:A :a)"));
    assert!(local("ObjectPropertyAssertion(:r :a :b)"));
    assert!(!local("SubClassOf(:A owl:Nothing)"));
    assert!(!local("SubClassOf(:A ObjectComplementOf(:B))"));
    assert!(!local("DisjointClasses(:A :B)"));
    assert!(!local("FunctionalObjectProperty(:r)"));
    assert!(!local("IrreflexiveObjectProperty(:r)"));
    assert!(!local("SubClassOf(:A ObjectMinCardinality(2 :r))"));
    assert!(!local(
        "SubClassOf(:A ObjectSomeValuesFrom(owl:bottomObjectProperty :B))"
    ));
    assert!(!local("DataPropertyAssertion(:d :a \"x\")"));
    assert!(!local("NegativeObjectPropertyAssertion(:r :a :b)"));
    assert!(!local("DifferentIndividuals(:a :b)"));
}

#[test]
fn past_its_bound_the_pass_leaves_the_undecided_classes_incomplete() {
    let (all, out) = read(IRL1);
    let all: Vec<&AxiomKind> = all.iter().collect();
    let out: Vec<&AxiomKind> = out.iter().collect();
    let classes: Vec<Class> = ["Ans", "D1", "D2", "K", "S"]
        .iter()
        .map(|c| Class::new(format!("http://e/#{c}")))
        .collect();
    for budget in 0..64 {
        let c = coverage(&all, &out, true, classes.iter(), budget, &|| false).unwrap();
        // whatever the bound, the classes called complete are a subset of the exact ones
        let exact = ["Ans", "D1", "D2"];
        for k in &classes {
            if !c.incomplete.contains(k) {
                assert!(
                    exact.iter().any(|e| k.as_str().ends_with(e)),
                    "{budget}: {k:?} called complete"
                );
            }
        }
        if c.over_budget {
            assert!(c.incomplete.len() >= 2, "{budget}: {c:?}");
        }
    }
    let c = coverage(&all, &out, true, classes.iter(), 0, &|| false).unwrap();
    assert!(c.over_budget && !c.consistency_known && c.incomplete.len() == 5);
    // a caller's stop ends the pass with an error
    let many: Vec<AxiomKind> = (0..10_000)
        .map(|i| {
            AxiomKind::SubClassOf(
                Class::new(format!("http://e/#A{i}")).into(),
                Class::new(format!("http://e/#A{}", i + 1)).into(),
            )
        })
        .collect();
    let mut with: Vec<&AxiomKind> = many.iter().collect();
    with.extend(all.iter().copied());
    assert_eq!(
        coverage(&with, &out, true, classes.iter(), usize::MAX, &|| true),
        Err(Stopped)
    );
}

/// The extraction with its triggers, its necessary conditions and its frozen base gives
/// the bottom module that a plain fixpoint over every axiom gives, for every class of the
/// 640 cases of roadmap item 603, over the whole ontology and over the terminology alone.
#[test]
fn the_fast_extraction_is_the_plain_fixpoint() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
    let mut cases: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".ofn") && n.matches('.').count() == 1)
        })
        .collect();
    cases.sort();
    assert_eq!(cases.len(), 640);
    let mut compared = 0usize;
    for case in &cases {
        let doc = crate::ofn_read::parse(&std::fs::read_to_string(case).unwrap()).unwrap();
        let kinds: Vec<&AxiomKind> = doc.axioms.iter().map(|a| &a.kind).collect();
        let ix = Index::new(&kinds);
        let mask = ix.terminology_gate();
        let mut classes: Vec<&str> = ix
            .names
            .iter()
            .map(|n| n.as_str())
            .filter(|n| doc.axioms.iter().any(|a| matches!(&a.kind, AxiomKind::Declaration(Entity::Class(c)) if c.as_str() == *n)))
            .collect();
        classes.sort();
        for include in [None, mask.as_ref()] {
            let mut base = Extraction::new(&ix, Mode::Bottom, include, None);
            base.extend(&[], &mut |_| Step::Go, &mut |_| Step::Go);
            base.freeze();
            for c in &classes {
                let id = ix.id(c).unwrap();
                base.begin();
                base.extend(&[id], &mut |_| Step::Go, &mut |_| Step::Go);
                let fast: Vec<usize> = (0..ix.axioms.len())
                    .filter(|&j| base.in_module(j))
                    .collect();
                // the plain fixpoint: every axiom checked again until none enters
                let mut sig: FxHashSet<String> = FxHashSet::from_iter([c.to_string()]);
                let mut taken = vec![false; ix.axioms.len()];
                loop {
                    let mut grew = false;
                    for j in 0..ix.axioms.len() {
                        if taken[j] || include.is_some_and(|m| !m[j]) {
                            continue;
                        }
                        let has = |iri: &str| sig.contains(iri);
                        if !bottom_local(ix.axioms[j], &Sig(&has)) {
                            taken[j] = true;
                            grew = true;
                            for &e in &ix.sigs[j] {
                                sig.insert(ix.names[e as usize].clone());
                            }
                        }
                    }
                    if !grew {
                        break;
                    }
                }
                let plain: Vec<usize> = (0..ix.axioms.len()).filter(|&j| taken[j]).collect();
                assert_eq!(
                    fast,
                    plain,
                    "{}: {c}, terminology {}",
                    case.display(),
                    include.is_some()
                );
                compared += 1;
            }
        }
    }
    assert!(compared > 5000, "{compared}");
}
