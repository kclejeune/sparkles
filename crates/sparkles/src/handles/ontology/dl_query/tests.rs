use super::*;
use crate::handles::ontology::classify::ClassifyRequest;
use serde_json::json;
use sparkles_owl::ObjectProperty;
use sparkles_owl::classify::Classifier;
use std::sync::Arc;

fn ex(local: &str) -> String {
    format!("http://ex.org/{local}")
}

fn c(local: &str) -> Class {
    Class::new(ex(local))
}

/// A dataset whose default graph holds the axioms of a functional-style document.
fn dataset(axioms: &str) -> Dataset {
    let doc = sparkles_owl::ofn_read::parse(&format!(
        "Prefix(:=<http://ex.org/>) Ontology(<http://ex.org/o> {axioms})"
    ))
    .unwrap();
    let ds = Dataset::memory();
    ds.update(
        "PREFIX : <http://ex.org/> INSERT DATA { :o a <http://www.w3.org/2002/07/owl#Ontology> }",
    )
    .unwrap();
    ds.ontology()
        .graph("default")
        .edit(|e| {
            for ax in &doc.axioms {
                e.add(ax.clone())?;
            }
            Ok(())
        })
        .unwrap();
    ds
}

fn query(ds: &Dataset, text: &str) -> DlQueryAnswer {
    let h = ds.ontology();
    let req = OntologyRequest::default();
    let opts = h.anchored(&Default::default()).unwrap();
    let o = h.open(&req, &opts).unwrap();
    let e = match h
        .parse_manchester(&o, text, What::ClassExpression, &opts)
        .unwrap()
    {
        Parsed::ClassExpression(e) => e,
        _ => unreachable!(),
    };
    h.dl_query_opened(o, &req, &DlQueryRequest::new(e), &opts)
        .unwrap()
}

const UNI: &str = "
    Declaration(Class(:Person)) Declaration(Class(:Professor)) Declaration(Class(:Course))
    Declaration(Class(:Teacher)) Declaration(Class(:Student)) Declaration(Class(:Lecturer))
    Declaration(Class(:Department)) Declaration(Class(:Unicorn))
    Declaration(ObjectProperty(:teaches)) Declaration(ObjectProperty(:partOf))
    AnnotationAssertion(rdfs:label :teaches \"teaches\")
    AnnotationAssertion(rdfs:label :Course \"Course\")
    SubClassOf(:Professor :Person)
    SubClassOf(:Professor ObjectSomeValuesFrom(:teaches :Course))
    SubClassOf(:Lecturer :Person)
    SubClassOf(:Lecturer ObjectSomeValuesFrom(:teaches :Course))
    SubClassOf(:Student :Person)
    EquivalentClasses(:Teacher ObjectIntersectionOf(:Person ObjectSomeValuesFrom(:teaches :Course)))
    SubClassOf(:Unicorn ObjectIntersectionOf(:Professor :Student))
    DisjointClasses(:Professor :Student)
    ClassAssertion(:Professor :ada)
    ObjectPropertyAssertion(:teaches :bob :logic)
    ClassAssertion(:Course :logic)
    ClassAssertion(:Person :bob)
";

#[test]
fn el_answers_from_the_kept_classifier() {
    let ds = dataset(UNI);
    let a = query(&ds, "Person and teaches some Course");
    assert_eq!(a.reasoning.engine, DlEngine::El);
    assert!(a.reasoning.complete, "{:?}", a.reasoning.gaps);
    assert!(a.incremental);
    assert_eq!(a.satisfiable, Some(true));
    assert_eq!(a.equivalents, Some(vec![c("Teacher")]));
    assert_eq!(a.direct_superclasses, Some(vec![c("Person")]));
    assert_eq!(a.superclasses, Some(vec![c("Person"), Class::thing()]));
    assert_eq!(
        a.direct_subclasses,
        Some(vec![c("Lecturer"), c("Professor")])
    );
    assert_eq!(a.subclasses, Some(vec![c("Lecturer"), c("Professor")]));
    // the instances come from a classification with the assertions
    assert_eq!(
        a.instances,
        Some(vec![
            Individual::named(ex("ada")),
            Individual::named(ex("bob"))
        ])
    );
    assert_eq!(a.consistent, Some(true));
    // an unsatisfiable expression is equivalent to owl:Nothing
    let u = query(&ds, "Professor and Student");
    assert_eq!(u.satisfiable, Some(false));
    assert_eq!(u.equivalents, Some(vec![c("Unicorn"), Class::nothing()]));
    assert_eq!(u.subclasses, Some(vec![]));
}

#[test]
fn a_query_leaves_the_kept_classification_as_it_was() {
    let ds = dataset(UNI);
    let h = ds.ontology();
    let req = OntologyRequest::default();
    let before = h
        .classify(&req, &ClassifyRequest::default(), &Default::default())
        .unwrap();
    for text in [
        "teaches some Course",
        "Person and teaches some Course",
        "Thing",
    ] {
        query(&ds, text);
    }
    let after = h
        .classify(&req, &ClassifyRequest::default(), &Default::default())
        .unwrap();
    assert!(after.reused);
    assert_eq!(
        before.classification.taxonomy.axioms(),
        after.classification.taxonomy.axioms()
    );
    // a classification at a later commit, after queries, equals a full one
    ds.update("PREFIX : <http://ex.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA { :Lecturer rdfs:subClassOf :Teacher }")
        .unwrap();
    query(&ds, "teaches some Course");
    let later = h
        .classify(&req, &ClassifyRequest::default(), &Default::default())
        .unwrap();
    let opened = h.open(&req, &Default::default()).unwrap();
    let mut full = Classifier::of(opened.ontology.axioms.keys());
    let full = full.classify(&Default::default()).unwrap();
    assert_eq!(
        later.classification.taxonomy.axioms(),
        full.taxonomy.axioms()
    );
    assert!(
        !later
            .classification
            .taxonomy
            .classes()
            .any(|k| k.as_str() == QUERY_CLASS)
    );
}

#[test]
fn el_answers_match_the_taxonomy_for_every_named_class() {
    let ds = dataset(&format!(
        "{UNI}
         SubObjectPropertyOf(ObjectPropertyChain(:teaches :partOf) :teaches)
         TransitiveObjectProperty(:partOf)
         SubClassOf(:Course ObjectSomeValuesFrom(:partOf :Department))
         EquivalentClasses(:DeptTeacher ObjectSomeValuesFrom(:teaches :Department))
         EquivalentClasses(:Tutor :Teacher)"
    ));
    let h = ds.ontology();
    let req = OntologyRequest::default();
    let cl = h
        .classify(&req, &ClassifyRequest::default(), &Default::default())
        .unwrap();
    let t = &cl.classification.taxonomy;
    let firsts = |ns: Vec<usize>| -> Vec<Class> {
        let mut v: Vec<Class> = ns
            .into_iter()
            .flat_map(|n| t.node(n).classes.clone())
            .collect();
        v.sort();
        v
    };
    for class in t.classes().cloned().collect::<Vec<_>>() {
        if t.is_unsatisfiable(&class) {
            continue;
        }
        let a = query(&ds, &format!("<{}>", class.as_str()));
        let n = t.node_of(&class).unwrap();
        // the equivalents of the expression `<C>` include C itself
        let mut eq: Vec<Class> = t.equivalents(&class).to_vec();
        eq.sort();
        assert_eq!(a.equivalents.as_ref().unwrap(), &eq, "{class:?}");
        assert_eq!(
            a.direct_superclasses.as_ref().unwrap(),
            &firsts(t.node(n).parents.clone()),
            "{class:?}"
        );
        assert_eq!(
            a.superclasses.as_ref().unwrap(),
            &firsts(t.ancestors(n)),
            "{class:?}"
        );
        assert_eq!(
            a.direct_subclasses.as_ref().unwrap(),
            &firsts(t.node(n).children.clone()),
            "{class:?}"
        );
        assert_eq!(
            a.subclasses.as_ref().unwrap(),
            &firsts(t.descendants(n)),
            "{class:?}"
        );
    }
}

#[test]
fn a_range_that_meets_an_individual_reaches_it() {
    // range(r) = C and r(a, b) put b in C, so Y ⊑ ∃s.{b} ⊑ ∃s.C with the assertions,
    // which the class answers do not read, and the terminology alone leaves b out of C
    let ds = dataset(
        "Declaration(Class(:C)) Declaration(Class(:Y)) Declaration(ObjectProperty(:r)) Declaration(ObjectProperty(:s))
         ObjectPropertyRange(:r :C)
         ObjectPropertyAssertion(:r :a :b)
         SubClassOf(:Y ObjectHasValue(:s :b))",
    );
    let a = query(&ds, "s some C");
    assert_eq!(a.reasoning.engine, DlEngine::El);
    assert_eq!(a.subclasses, Some(vec![]));
    assert!(a.reasoning.complete, "{:?}", a.reasoning.gaps);
    assert_eq!(a.reasoning.instances_complete, Some(true));
    // with the assertions, b is in C and an individual that reaches b is in ∃s.C
    let ds = dataset(
        "Declaration(Class(:C)) Declaration(ObjectProperty(:r)) Declaration(ObjectProperty(:s))
         ObjectPropertyRange(:r :C)
         ObjectPropertyAssertion(:r :a :b)
         ObjectPropertyAssertion(:s :d :b)",
    );
    let a = query(&ds, "s some C");
    assert!(a.reasoning.complete, "{:?}", a.reasoning.gaps);
    assert_eq!(a.instances, Some(vec![Individual::named(ex("d"))]));
}

#[test]
fn a_query_class_with_individuals_and_nominals_reads_its_world() {
    // T ≡ ∃r.{b} ⊓ ∃s.{b} puts b in C by range(r) = C where T has an instance, so
    // T ⊑ ∃s.C ≡ X holds in T's world only, and an expression below T is below X
    let ds = dataset(
        "Declaration(Class(:C)) Declaration(Class(:X)) Declaration(Class(:Y)) Declaration(Class(:T))
         Declaration(Class(:W)) Declaration(ObjectProperty(:r)) Declaration(ObjectProperty(:s))
         SubClassOf(:Y ObjectHasValue(:r :b))
         ObjectPropertyRange(:r :C)
         EquivalentClasses(:X ObjectSomeValuesFrom(:s :C))
         EquivalentClasses(:T ObjectIntersectionOf(ObjectHasValue(:r :b) ObjectHasValue(:s :b)))
         SubClassOf(:W ObjectHasValue(:s :b))
         ObjectPropertyAssertion(:s :d :b)",
    );
    let a = query(&ds, "Y and s value b");
    assert!(a.reasoning.complete, "{:?}", a.reasoning.gaps);
    assert_eq!(
        a.superclasses,
        Some(vec![c("T"), c("X"), c("Y"), Class::thing()])
    );
    assert_eq!(a.direct_superclasses, Some(vec![c("T"), c("Y")]));
    // d reaches b, which is in C only where something is in T, as d is not
    assert_eq!(a.instances, Some(vec![]));
    let x = query(&ds, "s some C");
    assert_eq!(x.equivalents, Some(vec![c("X")]));
    assert_eq!(x.subclasses, Some(vec![c("T")]));
    assert_eq!(x.instances, Some(vec![]));
    let b = query(&ds, "{b}");
    assert_eq!(b.superclasses, Some(vec![Class::thing()]));
    assert_eq!(b.instances, Some(vec![Individual::named(ex("b"))]));
    let s = query(&ds, "s value b");
    assert_eq!(s.subclasses, Some(vec![c("T"), c("W")]));
    assert_eq!(s.instances, Some(vec![Individual::named(ex("d"))]));
}

#[test]
fn outside_el_the_rl_rules_answer_by_demand() {
    let ds = dataset(
        "Declaration(Class(:A)) Declaration(Class(:B)) Declaration(Class(:D)) Declaration(Class(:E))
         Declaration(ObjectProperty(:p))
         SubClassOf(:A :B)
         SubClassOf(:D :B)
         ClassAssertion(:A :x)
         ClassAssertion(:D :y)
         ObjectPropertyAssertion(:p :z :x)",
    );
    let a = query(&ds, "A or D");
    assert_eq!(a.reasoning.engine, DlEngine::Rl);
    assert!(
        a.reasoning.why.contains("outside OWL 2 EL"),
        "{}",
        a.reasoning.why
    );
    assert!(!a.reasoning.complete);
    assert_eq!(a.subclasses, Some(vec![c("A"), c("D")]));
    assert_eq!(a.direct_subclasses, Some(vec![c("A"), c("D")]));
    assert_eq!(
        a.instances,
        Some(vec![Individual::named(ex("x")), Individual::named(ex("y"))])
    );
    assert_eq!(a.reasoning.instances_complete, Some(true));
    assert_eq!(a.satisfiable, None);
    // an expression the RL rules take only as a subclass is reported for the
    // superclasses
    assert!(
        a.reasoning
            .gaps
            .iter()
            .any(|g| g.axiom.is_none() && g.message.contains("superclass")),
        "{:?}",
        a.reasoning.gaps
    );
    let only = query(&ds, "p only A");
    assert_eq!(only.reasoning.engine, DlEngine::Rl);
    // an ontology with an axiom outside OWL 2 EL is answered by the RL rules too
    let ds = dataset(
        "Declaration(Class(:A)) Declaration(Class(:B)) Declaration(ObjectProperty(:p))
         SubClassOf(:A ObjectAllValuesFrom(:p :B))
         SubClassOf(:B :A)",
    );
    let a = query(&ds, "B");
    assert_eq!(a.reasoning.engine, DlEngine::Rl);
    assert!(a.reasoning.why.contains("1 axiom"), "{}", a.reasoning.why);
    assert_eq!(a.superclasses, Some(vec![c("A"), Class::thing()]));
}

#[test]
fn the_reasoning_can_be_named() {
    let ds = dataset(UNI);
    let h = ds.ontology();
    let mut q = DlQueryRequest::new(ClassExpression::some(
        ObjectProperty::new(ex("teaches")),
        c("Course"),
    ));
    q.reasoning = DlReasoning::Rl;
    q.answers = DlAnswers::none();
    q.answers.set("subclasses").unwrap();
    let a = h
        .dl_query(&OntologyRequest::default(), &q, &Default::default())
        .unwrap();
    assert_eq!(a.reasoning.engine, DlEngine::Rl);
    assert!(a.equivalents.is_none() && a.instances.is_none());
    // the RL rules derive the subsumptions through the told existentials
    let subs = a.subclasses.unwrap();
    assert!(
        subs.contains(&c("Professor")) && subs.contains(&c("Teacher")),
        "{subs:?}"
    );
    q.reasoning = DlReasoning::El;
    q.expression = ClassExpression::all(ObjectProperty::new(ex("teaches")), c("Course"));
    let e = h
        .dl_query(&OntologyRequest::default(), &q, &Default::default())
        .err()
        .unwrap();
    assert!(e.to_string().contains("outside OWL 2 EL"), "{e}");
}

#[test]
fn manchester_errors_carry_their_span_and_candidates() {
    let ds = dataset(
        "Declaration(Class(:Course)) Declaration(Class(<http://other.org/Course>))
         Declaration(ObjectProperty(:teaches))",
    );
    let h = ds.ontology();
    let o = h
        .open(&OntologyRequest::default(), &Default::default())
        .unwrap();
    let e = h
        .parse_manchester(
            &o,
            "teaches some Course",
            What::ClassExpression,
            &Default::default(),
        )
        .unwrap_err();
    let Error::Component(c) = &e else {
        panic!("{e:?}")
    };
    assert_eq!(c.code, "invalid-manchester");
    let f = c
        .source
        .as_ref()
        .unwrap()
        .downcast_ref::<ManchesterFailure>()
        .unwrap();
    let j = f.json();
    assert_eq!(j["reason"], "ambiguous");
    assert_eq!(
        (j["start"].clone(), j["end"].clone()),
        (json!(13), json!(19))
    );
    assert_eq!(j["candidates"].as_array().unwrap().len(), 2);
}

#[test]
fn the_wire_operations_answer() {
    let ds = dataset(UNI);
    let j = crate::handles::ontology::wire::call(
        &ds,
        "dlQuery",
        &json!({"expression": "teaches some Course", "answers": ["subclasses", "equivalents"]}),
        &Default::default(),
    )
    .unwrap();
    assert_eq!(j["reasoning"]["engine"], "el");
    assert_eq!(j["expression"]["manchester"], "teaches some Course");
    assert_eq!(
        j["subclasses"],
        json!([ex("Lecturer"), ex("Professor"), ex("Teacher")])
    );
    assert!(j.get("instances").is_none());
    assert_eq!(j["counts"]["subclasses"], 3);
    let p = crate::handles::ontology::wire::call(
        &ds,
        "parse",
        &json!({"text": "Professor SubClassOf teaches some Course", "as": "axiom"}),
        &Default::default(),
    )
    .unwrap();
    assert_eq!(p["value"]["type"], "SubClassOf");
    // a bare local name reads, and the dataset has no prefix for the IRI it renders
    assert_eq!(
        p["manchester"],
        "<http://ex.org/Professor> SubClassOf teaches some Course"
    );
    let e = crate::handles::ontology::wire::call(
        &ds,
        "dlQuery",
        &json!({"expression": "teaches some", }),
        &Default::default(),
    )
    .unwrap_err();
    assert!(e.to_string().contains("characters 12 to 12"), "{e}");
}

/// A query of Manchester `text` with the engine, the answers and the options given.
fn query_with(
    ds: &Dataset,
    text: &str,
    reasoning: DlReasoning,
    answers: &[&str],
    opts: &QueryOptions,
) -> Result<DlQueryAnswer> {
    let h = ds.ontology();
    let req = OntologyRequest::default();
    let opts = h.anchored(opts)?;
    let o = h.open(&req, &opts)?;
    let e = match h.parse_manchester(&o, text, What::ClassExpression, &opts)? {
        Parsed::ClassExpression(e) => e,
        _ => unreachable!(),
    };
    let mut q = DlQueryRequest::new(e);
    q.reasoning = reasoning;
    q.answers = DlAnswers::none();
    for a in answers {
        q.answers.set(a)?;
    }
    h.dl_query_opened(o, &req, &q, &opts)
}

#[test]
fn the_rl_copy_is_charged_to_the_memory_grant() {
    let ds = dataset("Declaration(Class(:A)) SubClassOf(:A ObjectUnionOf(:B :C))");
    let pool = sparkles_core::sparql::pool::QueryPool::new(1024);
    let grant = Arc::new(pool.try_grant(1024).unwrap());
    let opts = QueryOptions {
        grant: Some(grant.clone()),
        ..Default::default()
    };
    let Err(e) = query_with(&ds, "A", DlReasoning::Rl, &["superclasses"], &opts) else {
        panic!("an exhausted grant answered");
    };
    assert!(
        matches!(
            e,
            Error::BudgetExceeded(crate::Budget {
                kind: crate::BudgetKind::ServerMemory,
                ..
            })
        ),
        "{e}"
    );
    // a grant large enough is charged while the copy lives and released after
    let pool = sparkles_core::sparql::pool::QueryPool::new(64 << 20);
    let grant = Arc::new(pool.try_grant(64 << 20).unwrap());
    let opts = QueryOptions {
        grant: Some(grant.clone()),
        ..Default::default()
    };
    query_with(&ds, "A", DlReasoning::Rl, &["superclasses"], &opts).unwrap();
    assert_eq!(grant.used(), 0);
}

#[test]
fn a_chain_past_the_rule_set_leaves_the_rl_instances_incomplete() {
    // prp-spo2 covers chains of up to three properties, so r(a, e) and the domain C are
    // not derived for a chain of four
    let ds = dataset(
        "Declaration(Class(:C)) Declaration(ObjectProperty(:r))
         Declaration(ObjectProperty(:p1)) Declaration(ObjectProperty(:p2))
         Declaration(ObjectProperty(:p3)) Declaration(ObjectProperty(:p4))
         SubObjectPropertyOf(ObjectPropertyChain(:p1 :p2 :p3 :p4) :r)
         ObjectPropertyDomain(:r :C)
         ObjectPropertyAssertion(:p1 :a :b) ObjectPropertyAssertion(:p2 :b :c)
         ObjectPropertyAssertion(:p3 :c :d) ObjectPropertyAssertion(:p4 :d :e)",
    );
    let a = query_with(
        &ds,
        "C",
        DlReasoning::Rl,
        &["instances"],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(a.reasoning.instances_complete, Some(false));
    assert!(
        a.reasoning
            .gaps
            .iter()
            .any(|g| g.rule.as_deref() == Some("prp-spo2") && g.partial),
        "{:?}",
        a.reasoning.gaps
    );
}

#[test]
fn rl_superclasses_alone_leave_out_the_class_itself() {
    let ds = dataset("Declaration(Class(:A)) Declaration(Class(:B)) SubClassOf(:A :B)");
    let a = query_with(
        &ds,
        "A",
        DlReasoning::Rl,
        &["superclasses", "directSuperclasses"],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(a.direct_superclasses, Some(vec![c("B")]));
    assert!(!a.superclasses.unwrap().contains(&c("A")));
    assert_eq!(a.equivalents, None);
}

#[test]
fn a_declared_individual_is_an_instance_of_what_holds_for_everything() {
    let ds = dataset(
        "Declaration(Class(:A)) Declaration(NamedIndividual(:a))
         SubClassOf(<http://www.w3.org/2002/07/owl#Thing> :A)",
    );
    let a = query_with(
        &ds,
        "A",
        DlReasoning::El,
        &["instances"],
        &Default::default(),
    )
    .unwrap();
    assert_eq!(a.instances, Some(vec![Individual::named(ex("a"))]));
}

#[test]
fn names_only_assertions_hold_are_read() {
    // :k is a class and, through an assertion only, an individual too
    let ds = dataset(
        "Declaration(Class(:C)) Declaration(Class(:k)) Declaration(ObjectProperty(:p))
         ClassAssertion(:C :a) ObjectPropertyAssertion(:p :a :b) ObjectPropertyAssertion(:p :b :k)
         AnnotationAssertion(rdfs:label :b \"Ada Lovelace\")
         ObjectPropertyAssertion(:p :a <http://ex.org/ΟΣ>)",
    );
    for text in [
        "p value a",
        "p value b",
        "p value b and p value k",
        "k and p value k",
        // the lookup's own rules: names in another case, and labels with escapes
        "p value B",
        "p value 'Ada Lovelace'",
        "p value 'Ada\\u0020Lovelace'",
        // a final sigma lowers differently from the letter alone
        "p value ΟΣ",
        "p value ος",
    ] {
        let a = query_with(
            &ds,
            text,
            DlReasoning::Auto,
            &["subclasses"],
            &Default::default(),
        )
        .unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(a.reasoning.engine, DlEngine::El, "{text}");
    }
    let Err(e) = query_with(
        &ds,
        "p value nobody",
        DlReasoning::Auto,
        &[],
        &Default::default(),
    ) else {
        panic!("an unknown individual read");
    };
    assert!(e.to_string().contains("nobody"), "{e}");
}

#[test]
fn a_name_many_assertions_share_reads_as_ambiguous_within_a_bound() {
    // one entity, then two per assertion, so the bound falls inside an assertion
    let members: String = (0..200)
        .map(|n| {
            format!(
                "ObjectPropertyAssertion(:q <http://ex.org/g/{}/member> <http://ex.org/g/{}/member>) ",
                2 * n,
                2 * n + 1
            )
        })
        .collect();
    let ds = dataset(&format!(
        "Declaration(Class(:C)) Declaration(ObjectProperty(:p)) Declaration(ObjectProperty(:q))
         ClassAssertion(:C <http://ex.org/g/x/member>) {members}"
    ));
    let h = ds.ontology();
    let opts = h.anchored(&Default::default()).unwrap();
    let o = h.open(&OntologyRequest::default(), &opts).unwrap();
    let Err(Error::Component(c)) =
        h.parse_manchester(&o, "p value member", What::ClassExpression, &opts)
    else {
        panic!("a name 401 individuals share read");
    };
    let j = c
        .source
        .as_ref()
        .unwrap()
        .downcast_ref::<ManchesterFailure>()
        .unwrap()
        .json();
    assert_eq!(j["reason"], "ambiguous");
    let n = j["candidates"].as_array().unwrap().len();
    assert!((2..=256).contains(&n), "{n} candidates");
}

#[test]
fn a_query_refused_threads_leaves_the_kept_state_as_it_was() {
    let ds = dataset(
        "Declaration(Class(:A)) Declaration(Class(:B)) Declaration(Class(:Z)) SubClassOf(:A :B)",
    );
    let h = ds.ontology();
    let req = OntologyRequest::default();
    let c = |s: &str| Class::new(format!("http://ex.org/{s}"));
    let a = h
        .classify(&req, &ClassifyRequest::default(), &Default::default())
        .unwrap();
    assert!(a.classification.taxonomy.is_subclass(&c("A"), &c("B")));
    ds.update(
        "PREFIX : <http://ex.org/> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         DELETE DATA { :A rdfs:subClassOf :B }",
    )
    .unwrap();
    // a query whose wait for threads ends at the deadline gives the saturation nothing
    crate::handles::ontology::classify::REFUSE_ADMISSION.with(|r| r.set(true));
    let Err(e) = query_with(
        &ds,
        "Z",
        DlReasoning::Auto,
        &["subclasses"],
        &Default::default(),
    ) else {
        panic!("a refused query answered");
    };
    assert!(matches!(e, Error::Timeout), "{e}");
    let b = h
        .classify(&req, &ClassifyRequest::default(), &Default::default())
        .unwrap();
    assert!(!b.classification.taxonomy.is_subclass(&c("A"), &c("B")));
    let q = query(&ds, "B");
    assert_eq!(q.subclasses, Some(vec![]));
}

#[test]
fn the_kept_taxonomy_counts_beside_a_query_saturation() {
    let ds = dataset(UNI);
    let h = ds.ontology();
    h.classify(
        &OntologyRequest::default(),
        &ClassifyRequest::default(),
        &Default::default(),
    )
    .unwrap();
    let kept = || {
        ds.state()
            .classify_cache
            .lock()
            .entry
            .as_ref()
            .map(|e| e.taxonomy_memory())
    };
    let before = kept().unwrap();
    assert!(before > 0);
    query(&ds, "teaches some Course");
    assert_eq!(kept(), Some(before));
}
