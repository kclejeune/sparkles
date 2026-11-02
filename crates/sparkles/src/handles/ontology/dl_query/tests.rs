use super::*;
use crate::handles::ontology::classify::ClassifyRequest;
use serde_json::json;
use sparkles_owl::ObjectProperty;

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
    let e = match h.parse_manchester(&o, text, What::ClassExpression).unwrap() {
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
fn a_range_that_meets_an_individual_is_reported_as_incomplete() {
    // range(r) = C and r(a, b) put b in C, so Y ⊑ ∃s.{b} ⊑ ∃s.C, which both EL
    // reasoners miss (item 591)
    let ds = dataset(
        "Declaration(Class(:C)) Declaration(Class(:Y)) Declaration(ObjectProperty(:r)) Declaration(ObjectProperty(:s))
         ObjectPropertyRange(:r :C)
         ObjectPropertyAssertion(:r :a :b)
         SubClassOf(:Y ObjectHasValue(:s :b))",
    );
    let a = query(&ds, "s some C");
    assert_eq!(a.reasoning.engine, DlEngine::El);
    // known gap of item 587: Y is a subclass
    assert_eq!(a.subclasses, Some(vec![]));
    assert!(!a.reasoning.complete);
    assert!(
        a.reasoning
            .gaps
            .iter()
            .any(|g| g.partial && g.message.contains("individuals")),
        "{:?}",
        a.reasoning.gaps
    );
    assert_eq!(a.reasoning.instances_complete, Some(false));
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
        .parse_manchester(&o, "teaches some Course", What::ClassExpression)
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
