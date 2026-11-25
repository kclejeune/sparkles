use super::*;
use crate::ofn::Renderer;

fn ex_vocab() -> Vocabulary {
    let mut v = Vocabulary::new([("ex", "http://ex.org/")]);
    v.add_entity(Entity::Class(Class::new("http://ex.org/Organ")));
    v.add_entity(Entity::Class(Class::new("http://ex.org/Tissue")));
    v.add_entity(Entity::ObjectProperty(ObjectProperty::new(
        "http://ex.org/partOf",
    )));
    v.add_label("http://ex.org/partOf", "part of");
    v.add_entity(Entity::DataProperty(DataProperty::new(
        "http://ex.org/code",
    )));
    v
}

#[test]
fn robot_inline_template_row_classes_labels_subclasses_and_annotations() {
    let csv = "\
ID,Label,Type,Parent,PartOf,Synonyms,Comment,CommentSource
ID,LABEL@en,TYPE,SC %,SC ex:partOf some %,A skos:altLabel SPLIT=|,A rdfs:comment,>A dcterms:source
ex:Heart,Heart,owl:Class,ex:Organ,,Cardiac organ|Pump,Pumps blood,PMID:123
ex:Valve,Heart valve,owl:Class,ex:Tissue,ex:Heart,Cardiac valve,Controls flow,PMID:456
";
    let spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res = evaluate(csv, Delimiter::Auto, &spec, &ex_vocab(), &Limits::default()).unwrap();
    assert!(res.is_valid(), "diagnostics: {:?}", res.diagnostics);
    assert_eq!(res.rows_processed, 2);
    assert_eq!(res.rows_with_errors, 0);

    let r = Renderer::new([
        ("ex", "http://ex.org/"),
        ("skos", "http://www.w3.org/2004/02/skos/core#"),
        ("dcterms", "http://purl.org/dc/terms/"),
    ]);
    let ofns: Vec<String> = res.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();

    assert!(ofns.iter().any(|s| s == "Declaration(Class(ex:Heart))"));
    assert!(
        ofns.iter()
            .any(|s| s == "AnnotationAssertion(rdfs:label ex:Heart \"Heart\"@en)")
    );
    assert!(ofns.iter().any(|s| s == "SubClassOf(ex:Heart ex:Organ)"));
    assert!(
        ofns.iter()
            .any(|s| s == "AnnotationAssertion(skos:altLabel ex:Heart \"Cardiac organ\")")
    );
    assert!(
        ofns.iter()
            .any(|s| s == "AnnotationAssertion(skos:altLabel ex:Heart \"Pump\")")
    );
    assert!(
        ofns.iter()
            .any(|s| s == "SubClassOf(ex:Valve ObjectSomeValuesFrom(ex:partOf ex:Heart))")
    );
    // Axiom annotation attached to rdfs:comment
    assert!(ofns.iter().any(|s| {
        s.contains("Annotation(dcterms:source \"PMID:123\")")
            && s.contains("AnnotationAssertion(")
            && s.contains("\"Pumps blood\"")
    }));
    // Triples generated deterministically
    let t1 = res.triples();
    let t2 = res.triples();
    assert_eq!(t1, t2);
    assert!(!t1.is_empty());
}

#[test]
fn robot_tsv_explicit_columns_individuals_and_ec() {
    let tsv = "id\tlabel\tkind\tparent\teq\tinst\n\
ex:Aorta\tAorta\towl:Class\tex:Organ\tex:Organ and ex:partOf some ex:Heart\tex:aorta1\n\
ex:Heart\tHeart\towl:Class\tex:Organ\t\t\n";

    let spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: false,
            columns: vec![
                ColumnDirective {
                    column: "id".into(),
                    directive: "ID".into(),
                    split: None,
                },
                ColumnDirective {
                    column: "label".into(),
                    directive: "LABEL".into(),
                    split: None,
                },
                ColumnDirective {
                    column: "kind".into(),
                    directive: "TYPE".into(),
                    split: None,
                },
                ColumnDirective {
                    column: "parent".into(),
                    directive: "SC %".into(),
                    split: None,
                },
                ColumnDirective {
                    column: "eq".into(),
                    directive: "EC %".into(),
                    split: None,
                },
                ColumnDirective {
                    column: "inst".into(),
                    directive: "I %".into(),
                    split: None,
                },
            ],
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res = evaluate(tsv, Delimiter::Auto, &spec, &ex_vocab(), &Limits::default()).unwrap();
    assert_eq!(res.delimiter, Delimiter::Tab);
    assert!(res.is_valid(), "diagnostics: {:?}", res.diagnostics);

    let r = Renderer::new([("ex", "http://ex.org/")]);
    let ofns: Vec<String> = res.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();
    assert!(ofns.iter().any(|s| s.starts_with("EquivalentClasses(")
        && s.contains("ex:Aorta")
        && s.contains("ex:Heart")));
    assert!(
        ofns.iter()
            .any(|s| s == "ClassAssertion(ex:Aorta ex:aorta1)")
    );
}

#[test]
fn dosdp_pattern_rules_manchester_and_ofn_with_filters_and_split() {
    let csv = "\
cls,label,parent,parts,source
Heart,Heart,ex:Organ,ex:Valve|ex:Wall,curator-1
Valve,Heart \"Aortic\" Valve,ex:Tissue,,curator-2
";
    let mut split = BTreeMap::new();
    split.insert("parts".to_string(), "|".to_string());
    let spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: vec![
                ColumnDeclaration {
                    column: "cls".into(),
                    kind: EntityKind::Class,
                    split: None,
                },
                ColumnDeclaration {
                    column: "parts".into(),
                    kind: EntityKind::Class,
                    split: Some("|".into()),
                },
            ],
            rules: vec![
                PatternRule {
                    name: Some("subclass".into()),
                    template: "${cls|iri} SubClassOf ${parent|iri}".into(),
                    syntax: PatternSyntax::Manchester,
                    skip_if_empty: true,
                    required_columns: vec!["cls".into()],
                    split: BTreeMap::new(),
                    annotations: vec![RuleAnnotation {
                        property: "dcterms:creator".into(),
                        value: "${source}".into(),
                        lang: None,
                        datatype: None,
                        iri: false,
                    }],
                },
                PatternRule {
                    name: Some("part_restriction".into()),
                    template: "${cls|iri} SubClassOf ex:partOf some ${parts|iri}".into(),
                    syntax: PatternSyntax::Manchester,
                    skip_if_empty: true,
                    required_columns: Vec::new(),
                    split,
                    annotations: Vec::new(),
                },
                PatternRule {
                    name: Some("label_ofn".into()),
                    template: "AnnotationAssertion(rdfs:label ${cls|iri} ${label|lang(en)})".into(),
                    syntax: PatternSyntax::Functional,
                    skip_if_empty: true,
                    required_columns: Vec::new(),
                    split: BTreeMap::new(),
                    annotations: Vec::new(),
                },
            ],
        },
        base_iri: Some("http://ex.org/".into()),
        prefixes: BTreeMap::new(),
    };
    let res = evaluate(
        csv,
        Delimiter::Comma,
        &spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert!(res.is_valid(), "diagnostics: {:?}", res.diagnostics);
    assert_eq!(res.rows_processed, 2);

    let r = Renderer::new([
        ("ex", "http://ex.org/"),
        ("dcterms", "http://purl.org/dc/terms/"),
    ]);
    let ofns: Vec<String> = res.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();
    assert!(ofns.iter().any(|s| {
        s.contains("SubClassOf(Annotation(dcterms:creator \"curator-1\") ex:Heart ex:Organ)")
    }));
    assert!(
        ofns.iter()
            .any(|s| s == "SubClassOf(ex:Heart ObjectSomeValuesFrom(ex:partOf ex:Valve))")
    );
    assert!(
        ofns.iter()
            .any(|s| s == "SubClassOf(ex:Heart ObjectSomeValuesFrom(ex:partOf ex:Wall))")
    );
    assert!(
        ofns.iter()
            .any(|s| s
                == "AnnotationAssertion(rdfs:label ex:Valve \"Heart \\\"Aortic\\\" Valve\"@en)")
    );
}

#[test]
fn row_diagnostics_and_bounds_enforced() {
    let csv = "\
ID,Parent
ID,SC %
ex:Good,ex:Organ
bad iri with spaces,ex:Organ
ex:BadExpr,not valid ( syntax
";
    let spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res = evaluate(
        csv,
        Delimiter::Comma,
        &spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert!(!res.is_valid());
    assert_eq!(res.rows_with_errors, 2);
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.row == 4 && d.code == "invalid-iri")
    );
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.row == 5 && d.code == "invalid-expression")
    );

    // Enforce row limit
    let tight = Limits {
        max_rows: 1,
        ..Limits::default()
    };
    let err = evaluate(csv, Delimiter::Comma, &spec, &ex_vocab(), &tight).unwrap_err();
    assert_eq!(err.code, "too-many-rows");
}
