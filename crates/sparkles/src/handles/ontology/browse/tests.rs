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
         :a a owl:NamedIndividual, :A ; :p :b . :b a owl:NamedIndividual, :C .
         :p a owl:TransitiveProperty .
         [ a owl:Class ; owl:intersectionOf ( :B :C ) ] rdfs:subClassOf :D .
         [ a owl:Axiom ; owl:annotatedSource :B ; owl:annotatedProperty rdfs:label ;
           owl:annotatedTarget \"bird\" ; rdfs:comment \"from a book\" ] .
         :V a owl:Class ; rdfs:subClassOf _:r .
         _:r a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :C .
         [ a owl:Axiom ; owl:annotatedSource :V ; owl:annotatedProperty rdfs:subClassOf ;
           owl:annotatedTarget _:r ; rdfs:comment \"annotated restriction\" ] ."
    );
    let ds = dataset(&body);
    for e in ["A", "B", "C", "D", "V", "W", "X", "p", "a", "b", "Z"] {
        let iri = ex(e);
        let t = term(&ds, &iri);
        // every axiom of the answer mentions the entity: the types of the other IRIs
        // that the fragment reads to type them add none of their own
        for (a, _) in t.axioms.iter().chain(&t.mentions) {
            assert!(a.mentions(&iri), "{e}: {a:?} does not mention {iri}");
        }
        let got: BTreeSet<Axiom> = t
            .axioms
            .iter()
            .chain(&t.mentions)
            .map(|(a, _)| a.clone())
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
    // a scheme that only the hidden graph mentions is unknown, like any other IRI
    let r = ds.ontology().hierarchy(
        &both,
        &HierarchyRequest {
            kind: TreeKind::Concept,
            scheme: Some(ex("B")),
            ..Default::default()
        },
        &only_default,
    );
    assert!(matches!(r, Err(Error::NotFound(_))), "{r:?}");
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

#[test]
fn a_mention_keeps_the_annotations_of_its_axiom() {
    let ds = dataset(
        ":p a owl:ObjectProperty . :C a owl:Class .
         :V a owl:Class ; rdfs:subClassOf _:r .
         _:r a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :C .
         [ a owl:Axiom ; owl:annotatedSource :V ; owl:annotatedProperty rdfs:subClassOf ;
           owl:annotatedTarget _:r ; rdfs:comment \"annotated\" ] .",
    );
    let t = term(&ds, &ex("C"));
    assert_eq!(t.mentions.len(), 1, "{:?}", t.mentions);
    assert_eq!(t.mentions[0].0.annotations.len(), 1, "{:?}", t.mentions);
    assert!(full(&ds, &ex("C")).contains(&t.mentions[0].0));
}

#[test]
fn many_axioms_that_share_one_restriction_are_all_found() {
    let mut body = String::from(
        ":p a owl:ObjectProperty . :C a owl:Class . _:r a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :C .",
    );
    for n in 0..70 {
        body.push_str(&format!(" :S{n} a owl:Class ; rdfs:subClassOf _:r ."));
    }
    let ds = dataset(&body);
    let t = term(&ds, &ex("C"));
    assert_eq!(t.mentions_total, 70);
    assert_eq!(t.mentions.len(), 70);
    assert!(!t.mentions_truncated);
}

#[test]
fn skos_xl_labels_and_distinct_users() {
    let ds = dataset(
        ":c a skos:Concept ; skosxl:prefLabel :l1 ; skosxl:altLabel [ skosxl:literalForm \"other\"@en ] .
         :l1 a skosxl:Label ; skosxl:literalForm \"concept\"@en .
         :q a owl:ObjectProperty .
         :s1 :q :o1, :o2 . :s2 :q :o1 .",
    );
    let t = term(&ds, &ex("c"));
    let forms: Vec<String> = t
        .xl_labels
        .iter()
        .map(|s| format!("{} {:?}", s.property.rsplit('#').next().unwrap(), s.value))
        .collect();
    assert_eq!(forms.len(), 2, "{forms:?}");
    assert!(
        forms
            .iter()
            .any(|f| f.starts_with("prefLabel") && f.contains("concept"))
    );
    assert!(
        forms
            .iter()
            .any(|f| f.starts_with("altLabel") && f.contains("other"))
    );
    let u = term(&ds, &ex("q")).usage.unwrap();
    assert_eq!(u.count, 2);
    assert_eq!(u.subjects.len(), 2);
}

#[test]
fn a_scheme_root_ignores_broader_concepts_of_other_schemes() {
    let ds = dataset(
        ":s a skos:ConceptScheme . :t a skos:ConceptScheme .
         :a a skos:Concept ; skos:inScheme :s ; skos:broader :b .
         :b a skos:Concept ; skos:inScheme :t .",
    );
    let h = tree(
        &ds,
        HierarchyRequest {
            kind: TreeKind::Concept,
            scheme: Some(ex("s")),
            ..Default::default()
        },
    );
    assert_eq!(
        h.children.iter().map(|n| n.iri.clone()).collect::<Vec<_>>(),
        [ex("a")]
    );
}

#[test]
fn a_wide_level_comes_in_iri_order() {
    let mut body = String::from(":R a owl:Class .");
    // inserted in reverse, so the ids run against the IRIs
    for n in (0..20_005).rev() {
        body.push_str(&format!(" :K{n:05} a owl:Class ; rdfs:subClassOf :R ."));
    }
    let ds = dataset(&body);
    let h = tree(
        &ds,
        HierarchyRequest {
            iri: Some(ex("R")),
            limit: 3,
            ..Default::default()
        },
    );
    assert!(!h.sorted);
    assert_eq!(
        h.children.iter().map(|n| n.iri.clone()).collect::<Vec<_>>(),
        [ex("K00000"), ex("K00001"), ex("K00002")]
    );
}

#[test]
fn the_term_read_takes_the_requested_mode() {
    let ds = dataset(":A rdfs:subClassOf :B .");
    let sub = |mode: Mode| {
        let r = OntologyRequest {
            mode,
            ..Default::default()
        };
        ds.ontology()
            .term(&r, &TermRequest::new(ex("A")), &Default::default())
            .unwrap()
            .axioms
            .iter()
            .any(|(a, _)| matches!(a.kind, sparkles_owl::AxiomKind::SubClassOf(..)))
    };
    assert!(sub(Mode::Lenient));
    assert!(!sub(Mode::Strict));
}

#[test]
fn every_axiom_on_a_shared_target_keeps_its_annotations() {
    let mut body = String::from(
        ":p a owl:ObjectProperty . :C a owl:Class . _:r a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :C .",
    );
    for n in 0..600 {
        body.push_str(&format!(
            " :S{n} a owl:Class ; rdfs:subClassOf _:r .
              [ a owl:Axiom ; owl:annotatedSource :S{n} ; owl:annotatedProperty rdfs:subClassOf ;
                owl:annotatedTarget _:r ; rdfs:comment \"n{n}\" ] ."
        ));
    }
    let ds = dataset(&body);
    let r = TermRequest {
        mentions: 1000,
        ..TermRequest::new(ex("C"))
    };
    let t = ds.ontology().term(&req(), &r, &Default::default()).unwrap();
    assert_eq!(t.mentions.len(), 600);
    assert!(!t.mentions_truncated);
    assert!(
        t.mentions.iter().all(|(a, _)| a.annotations.len() == 1),
        "an axiom lost its annotation"
    );
}

#[test]
fn many_skos_xl_labels_report_the_cut() {
    let mut body = String::from(":c a skos:Concept .");
    for n in 0..(MAX_VALUES + 1) {
        body.push_str(&format!(
            " :c skosxl:altLabel [ skosxl:literalForm \"l{n}\"@en ] ."
        ));
    }
    let ds = dataset(&body);
    let t = term(&ds, &ex("c"));
    assert_eq!(t.xl_labels.len(), MAX_VALUES);
    assert!(t.statements_truncated);
}

fn individuals(ds: &Dataset, q: IndividualsRequest) -> Individuals {
    ds.ontology()
        .individuals(&req(), &q, &Default::default())
        .unwrap()
}

#[test]
fn individuals_page_memberships_without_losing_untyped_or_multiple_types() {
    let mut data = String::from(
        ":A a owl:Class ; rdfs:subClassOf :B . :B a owl:Class ; rdfs:subClassOf :A . :p a owl:ObjectProperty . :empty a owl:NamedIndividual . :onlySubject :p :onlyObject . :both a :A, :B . :A a owl:NamedIndividual, :A . [] a :A .",
    );
    for i in 0..1105 {
        data.push_str(&format!(":i{i:04} a :A ."));
    }
    let ds = dataset(&data);
    let roots = individuals(&ds, IndividualsRequest::default());
    assert!(roots.individuals.is_empty());
    assert_eq!(
        roots
            .groups
            .iter()
            .map(|g| (g.class.as_ref().map(|c| c.iri.clone()), g.total.unwrap()))
            .collect::<Vec<_>>(),
        vec![(None, 3), (Some(ex("A")), 1107), (Some(ex("B")), 1)]
    );
    let mut after = None;
    let mut all = Vec::new();
    loop {
        let page = individuals(
            &ds,
            IndividualsRequest {
                class: Some(ex("A")),
                limit: 100,
                after,
                ..Default::default()
            },
        );
        assert_eq!(page.total, 1107);
        assert!(page.individuals.len() <= 100);
        all.extend(page.individuals.iter().map(|i| i.name.iri.clone()));
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(all.len(), 1107);
    assert!(all.windows(2).all(|p| p[0] < p[1]));
    assert!(!all.iter().any(|i| i.starts_with("_:")));
    let both = individuals(
        &ds,
        IndividualsRequest {
            iri: Some(ex("both")),
            ..Default::default()
        },
    );
    assert_eq!(both.groups.len(), 2);
    assert_eq!(both.individuals[0].types.len(), 2);
    let untyped = individuals(
        &ds,
        IndividualsRequest {
            untyped: true,
            ..Default::default()
        },
    );
    assert_eq!(
        untyped
            .individuals
            .iter()
            .map(|i| i.name.iri.clone())
            .collect::<Vec<_>>(),
        [ex("empty"), ex("onlyObject"), ex("onlySubject")]
    );
    let root1 = individuals(
        &ds,
        IndividualsRequest {
            limit: 1,
            ..Default::default()
        },
    );
    assert_eq!(root1.next, Some(String::new()));
    let root2 = individuals(
        &ds,
        IndividualsRequest {
            limit: 1,
            after: root1.next,
            ..Default::default()
        },
    );
    assert_eq!(root2.groups[0].class.as_ref().unwrap().iri, ex("A"));
}

#[test]
fn individual_pages_keep_graph_scope_imports_and_retained_revisions() {
    let ds = dataset(
        "GRAPH :g { :g a owl:Ontology ; owl:imports :h . :A a owl:Class . :i a :A . } GRAPH :h { :i a :A . :hidden a :A . }",
    );
    ds.snapshots()
        .create(
            "before-individuals",
            &crate::history::At::Head,
            &Default::default(),
        )
        .unwrap();
    let req = OntologyRequest {
        graphs: vec![GraphRef::parse(&ex("g")).unwrap()],
        ..Default::default()
    };
    let q = IndividualsRequest {
        class: Some(ex("A")),
        limit: 1,
        ..Default::default()
    };
    let read = |r: &OntologyRequest, q: &IndividualsRequest, opts: &QueryOptions| {
        ds.ontology().individuals(r, q, opts).unwrap()
    };
    let page = read(&req, &q, &Default::default());
    assert_eq!(page.total, 2); // :i is counted once across the two graphs
    let no_imports = OntologyRequest {
        imports: sparkles_reasoner::inputs::ImportMode::None,
        ..req.clone()
    };
    assert_eq!(read(&no_imports, &q, &Default::default()).total, 1);
    let g = Graphs::Only(GraphRule::new([ex("g").as_str()], &[]));
    let opts = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(g.clone(), g))),
        ..Default::default()
    };
    assert_eq!(read(&req, &q, &opts).total, 1);
    ds.update(&format!(
        "{PREFIXES} INSERT DATA {{ GRAPH :g {{ :new a :A }} }}"
    ))
    .unwrap();
    assert_eq!(read(&req, &q, &Default::default()).total, 3);
    let retained = OntologyRequest {
        at: Some(crate::history::At::Snapshot("before-individuals".into())),
        ..req.clone()
    };
    let old = read(
        &retained,
        &IndividualsRequest {
            after: page.next,
            ..q.clone()
        },
        &Default::default(),
    );
    assert_eq!(old.commit, page.commit);
    assert_eq!(old.total, 2);
    assert_eq!(old.individuals.len(), 1);
    assert_eq!(old.individuals[0].name.iri, ex("i"));
}

#[test]
fn individual_pages_honor_cancellation_and_deadlines() {
    let ds = dataset(":a a owl:NamedIndividual .");
    let cancelled = QueryOptions {
        cancel: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        ..Default::default()
    };
    assert!(matches!(
        ds.ontology()
            .individuals(&req(), &Default::default(), &cancelled),
        Err(Error::Cancelled)
    ));
    let late = QueryOptions {
        deadline: Some(std::time::Instant::now()),
        ..Default::default()
    };
    assert!(matches!(
        ds.ontology()
            .individuals(&req(), &Default::default(), &late),
        Err(Error::Timeout)
    ));
}

#[test]
fn individual_pages_fail_explicitly_on_excessive_type_memberships() {
    let mut data = String::new();
    for n in 0..1001 {
        data.push_str(&format!(":a a :C{n} ."));
    }
    let ds = dataset(&data);
    let result = ds.ontology().individuals(
        &req(),
        &IndividualsRequest {
            iri: Some(ex("a")),
            ..Default::default()
        },
        &Default::default(),
    );
    assert!(
        matches!(result, Err(Error::Invalid(message)) if message.contains("more than 1000 named types"))
    );
}
