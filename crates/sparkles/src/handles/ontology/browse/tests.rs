use super::*;
use crate::Dataset;
use crate::access::{GraphAccess, GraphRule, Graphs};

const PREFIXES: &str = "PREFIX : <http://ex.org/>
PREFIX owl: <http://www.w3.org/2002/07/owl#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
PREFIX skosxl: <http://www.w3.org/2008/05/skos-xl#>
PREFIX oio: <http://www.geneontology.org/formats/oboInOwl#>
PREFIX obo: <http://purl.obolibrary.org/obo/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
";

fn ex(s: &str) -> String {
    format!("http://ex.org/{s}")
}

fn dataset(body: &str) -> Dataset {
    let ds = Dataset::memory();
    ds.update(&format!("{PREFIXES} INSERT DATA {{ {body} }}"))
        .unwrap();
    ds
}

/// A small ontology: a tree with two parents for D, a cycle between E and F, a
/// deprecated class, labels in two languages and a restriction.
const ONTO: &str = "
    :o a owl:Ontology .
    :p a owl:ObjectProperty ; rdfs:label \"part of\" .
    :A a owl:Class ; rdfs:label \"animal\" .
    :B a owl:Class ; rdfs:label \"bird\" ; rdfs:subClassOf :A .
    :C a owl:Class ; rdfs:label \"cat\"@en, \"chat\"@fr ; rdfs:subClassOf :A .
    :D a owl:Class ; rdfs:label \"dodo\" ; rdfs:subClassOf :B, :C, owl:Thing .
    :Z a owl:Class ; rdfs:label \"aardvark\" ; rdfs:subClassOf :A ; owl:deprecated true .
    :E a owl:Class ; rdfs:subClassOf :F . :F a owl:Class ; rdfs:subClassOf :E .
    :W a owl:Class ; rdfs:label \"wing\" ;
       rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
    :X a owl:Class ; rdfs:label \"x\" ;
       owl:equivalentClass [ a owl:Class ; owl:intersectionOf ( :A
         [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] ) ] .
";

fn req() -> OntologyRequest {
    OntologyRequest::default()
}

fn tree(ds: &Dataset, h: HierarchyRequest) -> Hierarchy {
    ds.ontology()
        .hierarchy(&req(), &h, &Default::default())
        .unwrap()
}

fn labels(nodes: &[Node]) -> Vec<String> {
    nodes
        .iter()
        .map(|n| n.label.clone().unwrap_or_else(|| n.iri.clone()))
        .collect()
}

#[test]
fn roots_children_and_their_order() {
    let ds = dataset(ONTO);
    let roots = tree(&ds, HierarchyRequest::default());
    assert_eq!(roots.roots_from, Some(RootsFrom::Computed));
    // classes without a parent, named ones by label, unlabelled ones by IRI last
    assert_eq!(labels(&roots.children), ["animal", "wing", "x"]);
    let a = &roots.children[0];
    assert_eq!((a.children, a.parents), (3, 0));

    let kids = tree(
        &ds,
        HierarchyRequest {
            iri: Some(ex("A")),
            ..Default::default()
        },
    );
    // deprecated last, owl:Thing ignored as a parent
    assert_eq!(labels(&kids.children), ["bird", "cat", "aardvark"]);
    assert!(kids.children[2].deprecated);
    assert_eq!(kids.total, 3);
    assert!(kids.sorted);
    let page = tree(
        &ds,
        HierarchyRequest {
            iri: Some(ex("A")),
            offset: 1,
            limit: 1,
            deprecated: false,
            ..Default::default()
        },
    );
    assert_eq!(labels(&page.children), ["cat"]);
    assert_eq!(page.total, 2);
    // the label follows the caller's language
    let fr = tree(
        &ds,
        HierarchyRequest {
            iri: Some(ex("C")),
            languages: vec!["fr".into()],
            ..Default::default()
        },
    );
    assert_eq!(fr.node.unwrap().label.as_deref(), Some("chat"));
}

#[test]
fn ancestors_follow_every_parent_and_stop_at_cycles() {
    let ds = dataset(ONTO);
    let h = tree(
        &ds,
        HierarchyRequest {
            iri: Some(ex("D")),
            ancestors: true,
            ..Default::default()
        },
    );
    let a = h.ancestors.unwrap();
    let mut got = labels(&a.nodes);
    got.sort();
    assert_eq!(got, ["animal", "bird", "cat"]);
    assert_eq!(a.edges.len(), 4);
    assert_eq!(h.node.unwrap().parents, 2);
    let cyc = tree(
        &ds,
        HierarchyRequest {
            iri: Some(ex("E")),
            ancestors: true,
            ..Default::default()
        },
    );
    assert_eq!(cyc.ancestors.unwrap().nodes.len(), 1);
    // an IRI the graphs do not use is not found
    let e = ds.ontology().hierarchy(
        &req(),
        &HierarchyRequest {
            iri: Some(ex("nothing")),
            ..Default::default()
        },
        &Default::default(),
    );
    assert!(matches!(e, Err(Error::NotFound(_))));
}

#[test]
fn the_header_root_terms_come_first() {
    let ds = dataset(&format!("{ONTO} :o obo:IAO_0000700 :A ."));
    let roots = tree(&ds, HierarchyRequest::default());
    assert_eq!(roots.roots_from, Some(RootsFrom::Ontology));
    assert_eq!(labels(&roots.children), ["animal"]);
}

#[test]
fn computed_roots_leave_out_deprecated_ones_on_request() {
    let ds = dataset(":A a owl:Class . :Old a owl:Class ; owl:deprecated true .");
    let all = tree(&ds, HierarchyRequest::default());
    assert_eq!(all.total, 2);
    let current = tree(
        &ds,
        HierarchyRequest {
            deprecated: false,
            ..Default::default()
        },
    );
    assert_eq!(current.total, 1);
    assert_eq!(current.deprecated_roots, 1);
}

#[test]
fn concepts_follow_broader_and_narrower_either_way() {
    let ds = dataset(
        ":s a skos:ConceptScheme ; skos:hasTopConcept :top .
         :top a skos:Concept ; skos:prefLabel \"top\"@en ; skos:inScheme :s .
         :a a skos:Concept ; skos:prefLabel \"a\"@en ; skos:broader :top ; skos:inScheme :s .
         :top skos:narrower :b .
         :b a skos:Concept ; skosxl:prefLabel [ skosxl:literalForm \"b\"@en ] ; skos:inScheme :s .
         :other a skos:Concept ; skos:prefLabel \"other\"@en .",
    );
    let kids = tree(
        &ds,
        HierarchyRequest {
            kind: TreeKind::Concept,
            iri: Some(ex("top")),
            ..Default::default()
        },
    );
    assert_eq!(labels(&kids.children), ["a", "b"]);
    assert_eq!(kids.children[1].parents, 1);
    let roots = tree(
        &ds,
        HierarchyRequest {
            kind: TreeKind::Concept,
            scheme: Some(ex("s")),
            ..Default::default()
        },
    );
    assert_eq!(roots.roots_from, Some(RootsFrom::Scheme));
    assert_eq!(labels(&roots.children), ["top"]);
    let all = tree(
        &ds,
        HierarchyRequest {
            kind: TreeKind::Concept,
            ..Default::default()
        },
    );
    assert_eq!(labels(&all.children), ["other", "top"]);
}

#[test]
fn property_hierarchies() {
    let ds = dataset(
        ":p a owl:ObjectProperty . :q a owl:ObjectProperty ; rdfs:subPropertyOf :p .
         :r a owl:ObjectProperty ; rdfs:subPropertyOf owl:topObjectProperty .",
    );
    let roots = tree(
        &ds,
        HierarchyRequest {
            kind: TreeKind::ObjectProperty,
            ..Default::default()
        },
    );
    assert_eq!(labels(&roots.children), [ex("p"), ex("r")]);
    assert_eq!(roots.children[0].children, 1);
}

fn term(ds: &Dataset, iri: &str) -> TermPage {
    ds.ontology()
        .term(&req(), &TermRequest::new(iri), &Default::default())
        .unwrap()
}

/// The axioms a full read gives that mention `iri`, as functional-style text.
fn full(ds: &Dataset, iri: &str) -> BTreeSet<Axiom> {
    let o = ds.ontology().open(&req(), &Default::default()).unwrap();
    let mut out: BTreeSet<Axiom> = o
        .ontology
        .axiom_set()
        .filter(|a| a.mentions(iri))
        .cloned()
        .collect();
    o.assertions(&Default::default(), |ax, _, _| {
        if let Some(ax) = ax
            && ax.mentions(iri)
        {
            out.insert(ax);
        }
        Ok(true)
    })
    .unwrap();
    out
}

#[test]
fn the_fragment_maps_to_the_axioms_of_a_full_read() {
    let body = format!(
        "{ONTO}
         :a a owl:NamedIndividual, :A ; :p :b . :b a owl:NamedIndividual .
         [ a owl:Class ; owl:intersectionOf ( :B :C ) ] rdfs:subClassOf :D .
         [ a owl:Axiom ; owl:annotatedSource :B ; owl:annotatedProperty rdfs:label ;
           owl:annotatedTarget \"bird\" ; rdfs:comment \"from a book\" ] ."
    );
    let ds = dataset(&body);
    for e in ["A", "B", "C", "D", "W", "X", "p", "a", "b", "Z"] {
        let iri = ex(e);
        let t = term(&ds, &iri);
        let got: BTreeSet<Axiom> = t
            .axioms
            .iter()
            .chain(&t.mentions)
            .map(|(a, _)| a.clone())
            .filter(|a| a.mentions(&iri))
            .collect();
        let want = full(&ds, &iri);
        assert!(
            got.is_subset(&want),
            "the axioms of {e}: {got:?} not in {want:?}"
        );
        // what the fragment leaves out comes from triples of named subjects that point
        // at the entity, which the answer lists as incoming statements
        let incoming: BTreeSet<&str> = t
            .incoming
            .iter()
            .chain(&t.usage)
            .flat_map(|i| i.subjects.iter().map(String::as_str))
            .collect();
        for a in want.difference(&got) {
            assert!(
                a.signature()
                    .iter()
                    .any(|s| incoming.contains(s.iri().as_str())),
                "the axiom {a:?} of {e} is neither read nor listed as incoming"
            );
        }
    }
}

#[test]
fn statements_annotations_mentions_and_incoming() {
    let ds = dataset(
        ":o a owl:Ontology .
         oio:hasExactSynonym a owl:AnnotationProperty .
         :p a owl:ObjectProperty ; rdfs:label \"part of\" .
         :M a owl:Class ; rdfs:label \"mitochondrion\" ; oio:id \"GO:1\" ;
            oio:hasExactSynonym \"mitochondria\" ; obo:IAO_0000115 \"An organelle.\" .
         [ a owl:Axiom ; owl:annotatedSource :M ; owl:annotatedProperty obo:IAO_0000115 ;
           owl:annotatedTarget \"An organelle.\" ; oio:hasDbXref \"ISBN:1\" ] .
         :Env a owl:Class ; rdfs:label \"envelope\" ;
            rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :M ] .
         :Sub a owl:Class ; rdfs:subClassOf :M .
         :i1 a :M . :i2 a :M .",
    );
    let t = term(&ds, &ex("M"));
    assert_eq!(t.name.label.as_deref(), Some("mitochondrion"));
    assert_eq!(t.name.short_form.as_deref(), Some("GO:1"));
    assert_eq!(t.name.kinds, ["class"]);
    assert_eq!(t.ontologies, [Some(ex("o"))]);
    let def = t
        .statements
        .iter()
        .find(|s| s.property.ends_with("IAO_0000115"))
        .unwrap();
    assert_eq!(def.annotations.len(), 1);
    assert_eq!(
        def.annotations[0].1,
        Value::Literal(oxrdf::Literal::new_simple_literal("ISBN:1"))
    );
    // the restriction of Env mentions M
    assert_eq!(t.mentions_total, 1);
    assert_eq!(t.mentions.len(), 1);
    assert!(t.mentions[0].0.mentions(&ex("Env")));
    assert_eq!(t.names[&ex("Env")].label.as_deref(), Some("envelope"));
    assert_eq!(t.names[&ex("p")].label.as_deref(), Some("part of"));
    // instances and subclasses come in as incoming statements
    let types = t
        .incoming
        .iter()
        .find(|i| i.property.ends_with("#type"))
        .unwrap();
    assert_eq!(types.count, 2);
    let subs = t
        .incoming
        .iter()
        .find(|i| i.property.ends_with("#subClassOf"))
        .unwrap();
    assert_eq!(subs.subjects, [ex("Sub")]);
    // the reification is part of the download, the mention is not
    assert!(
        t.quads
            .iter()
            .any(|q| q.predicate.as_str().ends_with("annotatedSource"))
    );
    assert!(
        !t.quads
            .iter()
            .any(|q| q.subject.to_string().contains("Env"))
    );
    let none = ds.ontology().term(
        &req(),
        &TermRequest::new(ex("nothing")),
        &Default::default(),
    );
    assert!(matches!(none, Err(Error::NotFound(_))));
}

#[test]
fn grants_hide_graphs_and_triples() {
    let ds = dataset(
        ":A a owl:Class ; rdfs:label \"a\" .
         GRAPH :g { :B a owl:Class ; rdfs:subClassOf :A ; rdfs:label \"secret\" .
                    :A rdfs:comment \"hidden\" . }",
    );
    let both = OntologyRequest {
        graphs: vec![
            GraphRef::Default,
            GraphRef::parse("http://ex.org/g").unwrap(),
        ],
        ..Default::default()
    };
    let g = Graphs::Only(GraphRule::new(["default"], &[]));
    let only_default = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(g.clone(), g))),
        ..Default::default()
    };
    let h = ds
        .ontology()
        .hierarchy(
            &both,
            &HierarchyRequest {
                iri: Some(ex("A")),
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    assert_eq!(h.total, 1);
    let h = ds
        .ontology()
        .hierarchy(
            &both,
            &HierarchyRequest {
                iri: Some(ex("A")),
                ..Default::default()
            },
            &only_default,
        )
        .unwrap();
    assert_eq!(h.total, 0);
    let t = ds
        .ontology()
        .term(&both, &TermRequest::new(ex("A")), &only_default)
        .unwrap();
    assert!(!t.statements.iter().any(|s| s.property.ends_with("comment")));
    assert!(t.incoming.is_empty());
    // a hidden predicate
    let hiding =
        crate::handles::ontology::tests::hiding("http://www.w3.org/2000/01/rdf-schema#comment");
    let t = ds
        .ontology()
        .term(&both, &TermRequest::new(ex("A")), &hiding)
        .unwrap();
    assert!(!t.statements.iter().any(|s| s.property.ends_with("comment")));
    assert!(t.statements.iter().any(|s| s.property.ends_with("label")));
}

#[test]
fn history_branches_and_inferences() {
    let ds = dataset(":A a owl:Class .");
    ds.snapshots()
        .create(
            "before",
            &crate::history::At::Head,
            &crate::history::SnapshotOptions::default(),
        )
        .unwrap();
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{ :B a owl:Class ; rdfs:subClassOf :A }}"
    ))
    .unwrap();
    let head = tree(
        &ds,
        HierarchyRequest {
            iri: Some(ex("A")),
            ..Default::default()
        },
    );
    assert_eq!(head.total, 1);
    let first = ds
        .ontology()
        .hierarchy(
            &OntologyRequest {
                at: Some(crate::history::At::Snapshot("before".into())),
                ..Default::default()
            },
            &HierarchyRequest {
                iri: Some(ex("A")),
                ..Default::default()
            },
            &Default::default(),
        )
        .unwrap();
    assert_eq!(first.total, 0);
    let inf = OntologyRequest {
        inferences: true,
        ..Default::default()
    };
    assert!(
        ds.ontology()
            .hierarchy(&inf, &HierarchyRequest::default(), &Default::default())
            .is_err()
    );
}
