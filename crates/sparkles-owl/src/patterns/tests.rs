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

#[test]
fn utf8_non_ascii_directives_and_filters_do_not_panic() {
    // Non-ASCII directive straddling byte index 6 ("LABEL@".len())
    let csv = "ID,Col\nID,a€€€\nex:Heart,x\n";
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
    assert!(res.has_fatal_error());
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.code == "invalid-directive")
    );

    // Non-ASCII filter straddling byte index 4 ("lang".len()), plus empty lang()/type()
    let p_csv = "cls\nex:Heart\n";
    let p_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: Vec::new(),
            rules: vec![
                PatternRule::manchester("${cls|a€€€(en)}"),
                PatternRule::manchester("${cls|lang()}"),
                PatternRule::manchester("${cls|type()}"),
            ],
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res = evaluate(
        p_csv,
        Delimiter::Comma,
        &p_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert!(!res.is_valid());
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.message.contains("unknown placeholder filter"))
    );
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.message.contains("requires a language tag"))
    );
    assert!(
        res.diagnostics
            .iter()
            .any(|d| d.message.contains("requires a datatype"))
    );
}

#[test]
fn bounds_robot_split_annotations_work_and_cancellation() {
    // Quadratic SPLIT x >A blowup is stopped cleanly by per-cell split & work limits
    let vals = (0..30)
        .map(|i| format!("ex:C{i}"))
        .collect::<Vec<_>>()
        .join("|");
    let anns = (0..30)
        .map(|i| format!("ref{i}"))
        .collect::<Vec<_>>()
        .join("|");
    let csv = format!(
        "ID,Parents,Sources\nID,SC % SPLIT=|,>A dcterms:source SPLIT=|\nex:Heart,{vals},{anns}\n"
    );
    let spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let tight = Limits {
        max_split_values_per_cell: 8,
        max_annotations_per_axiom: 4,
        ..Limits::default()
    };
    let res = evaluate(&csv, Delimiter::Comma, &spec, &ex_vocab(), &tight).unwrap();
    assert!(!res.is_valid());
    assert!(res.diagnostics.iter().any(|d| d.code == "expansion-limit"));
    // Failed row discards all staged axioms (including declaration)
    assert!(res.axioms.is_empty());

    // Request-wide work-limit triggers even on duplicate axioms
    let dup_csv = "ID,Parent\nID,SC %\nex:Heart,ex:Organ\nex:Heart,ex:Organ\nex:Heart,ex:Organ\n";
    let work_tight = Limits {
        max_axiom_attempts: 3,
        ..Limits::default()
    };
    let res = evaluate(dup_csv, Delimiter::Comma, &spec, &ex_vocab(), &work_tight).unwrap();
    assert!(!res.is_valid());
    assert!(res.budget_exceeded);
    assert!(res.has_fatal_error());
    assert!(res.diagnostics.iter().any(|d| d.code == "work-limit"));

    // Cooperative cancellation stops evaluation cleanly
    let mut ticks = 0usize;
    let err = evaluate_with_cancel(
        dup_csv,
        Delimiter::Comma,
        &spec,
        &ex_vocab(),
        &Limits::default(),
        &mut || {
            ticks += 1;
            if ticks >= 3 {
                Err(CancelReason::Timeout)
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    assert_eq!(err, EvalError::Timeout);
}

#[test]
fn undeclared_curie_prefix_rejected_while_full_iris_allowed() {
    let csv = "\
ID,Parent
ID,SC %
ex:Good,ex:Organ
exx:Typo,ex:Organ
ex:BadParent,exx:Organ
http://ex.org/FullHttp,urn:example:target
<http://ex.org/Bracketed>,ex:Organ
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
    // Good, FullHttp, and Bracketed succeed; exx:Typo and ex:BadParent are rejected & discarded
    let r = Renderer::new([("ex", "http://ex.org/")]);
    let ofns: Vec<String> = res.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();
    assert!(ofns.iter().any(|s| s == "SubClassOf(ex:Good ex:Organ)"));
    assert!(
        ofns.iter()
            .any(|s| s == "SubClassOf(ex:FullHttp <urn:example:target>)")
    );
    assert!(
        ofns.iter()
            .any(|s| s == "SubClassOf(ex:Bracketed ex:Organ)")
    );
    assert!(!ofns.iter().any(|s| s.contains("exx:")));
    assert!(!ofns.iter().any(|s| s.contains("BadParent")));
}

#[test]
fn ofn_functional_rules_preserve_multiline_tab_quote_and_backslash() {
    let csv = "cls,text\nHeart,\"line1\nline2\t\"\"quoted\"\"\\end\"\n";
    let spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: vec![ColumnDeclaration {
                column: "cls".into(),
                kind: EntityKind::Class,
                split: None,
            }],
            rules: vec![
                PatternRule::functional("AnnotationAssertion(rdfs:comment ${cls|iri} ${text|str})"),
                PatternRule::functional(
                    "AnnotationAssertion(rdfs:label ${cls|iri} ${text|lang(en)})",
                ),
                PatternRule::functional(
                    "AnnotationAssertion(skos:definition ${cls|iri} ${text|type(xsd:string)})",
                ),
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
    let expected = "line1\nline2\t\"quoted\"\\end";
    let lits: Vec<&str> = res
        .axioms
        .iter()
        .filter_map(|g| match &g.axiom.kind {
            AxiomKind::AnnotationAssertion(_, _, AnnotationValue::Literal(l)) => Some(l.value()),
            _ => None,
        })
        .collect();
    assert_eq!(lits.len(), 3);
    for lit in lits {
        assert_eq!(lit, expected);
    }
}

#[test]
fn robot_ec_intersection_ti_directive_and_pass1_split_type_label() {
    let csv = "\
ID,Label,Type,Eq1,Eq2,EqSrc,TiCol
ID,LABEL SPLIT=|,TYPE SPLIT=|,EC %,EC ex:partOf some %,>A dcterms:source,TI %
ex:HeartValve,Valve|Cardiac valve,owl:Class,ex:Organ,ex:Tissue,PMID:1,
ex:clinic1,Main Clinic,ex:Organ|ex:partOf some ex:Tissue,,,,ex:Tissue
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
    assert!(res.is_valid(), "diagnostics: {:?}", res.diagnostics);

    let r = Renderer::new([
        ("ex", "http://ex.org/"),
        ("dcterms", "http://purl.org/dc/terms/"),
    ]);
    let ofns: Vec<String> = res.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();

    // EC combines both expressions into a single EquivalentClasses(ObjectIntersectionOf(...))
    let ec_axioms: Vec<&String> = ofns
        .iter()
        .filter(|s| s.starts_with("EquivalentClasses("))
        .collect();
    assert_eq!(ec_axioms.len(), 1, "expected 1 EC axiom, got {ec_axioms:?}");
    assert!(
        ec_axioms[0].contains("ObjectIntersectionOf("),
        "{ec_axioms:?}"
    );
    assert!(
        ec_axioms[0].contains("Annotation(dcterms:source \"PMID:1\")"),
        "{ec_axioms:?}"
    );

    // Split TYPE with class expression + TI declares clinic1 as NamedIndividual (not Class)
    assert!(
        ofns.iter()
            .any(|s| s == "Declaration(NamedIndividual(ex:clinic1))"),
        "{ofns:?}"
    );
    assert!(
        !ofns.iter().any(|s| s == "Declaration(Class(ex:clinic1))"),
        "{ofns:?}"
    );
    assert!(
        ofns.iter()
            .any(|s| s == "ClassAssertion(ex:Organ ex:clinic1)"),
        "{ofns:?}"
    );
    assert!(
        ofns.iter()
            .any(|s| s == "ClassAssertion(ObjectSomeValuesFrom(ex:partOf ex:Tissue) ex:clinic1)"),
        "{ofns:?}"
    );
    assert!(
        ofns.iter()
            .any(|s| s == "ClassAssertion(ex:Tissue ex:clinic1)"),
        "{ofns:?}"
    );

    // Conflicting entity kinds on a row are diagnosed as conflicting-type
    let conflict_csv = "ID,Type\nID,TYPE SPLIT=|\nex:Bad,owl:Class|owl:ObjectProperty\n";
    let cres = evaluate(
        conflict_csv,
        Delimiter::Comma,
        &spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert!(!cres.is_valid());
    assert!(
        cres.diagnostics
            .iter()
            .any(|d| d.code == "conflicting-type")
    );
    assert!(cres.axioms.is_empty());

    // Unsupported CI directive is rejected explicitly
    let ci_csv = "ID,X\nID,CI %\nex:A,ex:B\n";
    let cires = evaluate(
        ci_csv,
        Delimiter::Comma,
        &spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert!(!cires.is_valid());
    assert!(
        cires
            .diagnostics
            .iter()
            .any(|d| d.code == "invalid-directive")
    );
}

#[test]
fn robot_merges_explicit_column_overrides_into_inline_template_row() {
    let csv = "\
ID,Parent,Source
ID,SC %,>A dcterms:source
ex:Heart,ex:Organ,PMID:9
";
    // Override only `Parent` with an existential restriction; `ID` and `>A` from row 2 stay intact!
    let spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: vec![ColumnDirective {
                column: "Parent".into(),
                directive: "SC ex:partOf some %".into(),
                split: None,
            }],
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
    assert!(res.is_valid(), "diagnostics: {:?}", res.diagnostics);
    let r = Renderer::new([
        ("ex", "http://ex.org/"),
        ("dcterms", "http://purl.org/dc/terms/"),
    ]);
    let ofns: Vec<String> = res.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();
    assert!(ofns.iter().any(|s| {
        s.contains("SubClassOf(Annotation(dcterms:source \"PMID:9\") ex:Heart ObjectSomeValuesFrom(ex:partOf ex:Organ))")
    }), "{ofns:?}");
}

#[test]
fn row_atomicity_discards_failed_row_declarations_annotations_and_shared_provenance() {
    // Row 3 succeeds and emits SubClassOf(ex:Heart ex:Organ).
    // Row 4 shares SubClassOf(ex:Heart ex:Organ) via Parent1, declares ex:Heart, AND fails
    // on an invalid axiom annotation (>AI with invalid prefix). Row 4 must be completely
    // discarded so neither unannotated axioms nor row 4 provenance leak.
    let csv = "\
ID,Parent,Source
ID,SC %,>AI dcterms:source
ex:Heart,ex:Organ,ex:Ref1
ex:Heart,ex:Organ,unknownprefix:BadRef
ex:Other,ex:Organ,unknownprefix:BadRef
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
    let r = Renderer::new([("ex", "http://ex.org/")]);
    // ex:Other's declaration must NOT appear because its row failed
    assert!(
        !res.axioms
            .iter()
            .any(|g| r.axiom(&g.axiom).contains("ex:Other"))
    );
    // Shared Declaration(Class(ex:Heart)) must carry ONLY row 3 provenance, not row 4
    let heart_decl = res
        .axioms
        .iter()
        .find(|g| r.axiom(&g.axiom) == "Declaration(Class(ex:Heart))")
        .expect("row 3 Heart declaration present");
    assert_eq!(heart_decl.rows, vec![3]);
}

#[test]
fn fail_closed_when_diagnostic_cap_filled_by_warnings_and_misc_p3_fixes() {
    // 1. Table extra-column warnings fill max_diagnostics=1, followed by an invalid directive
    //    error. Even though the error diagnostic is truncated, is_valid() is false and
    //    has_fatal_error() is true!
    let csv = "ID,Col\nID,NO_SUCH_DIRECTIVE\nex:A,x,extra1\nex:B,y,extra2\n";
    let spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let tiny_diag = Limits {
        max_diagnostics: 1,
        ..Limits::default()
    };
    let res = evaluate(csv, Delimiter::Comma, &spec, &ex_vocab(), &tiny_diag).unwrap();
    assert!(res.diagnostics_truncated);
    assert!(!res.is_valid());
    assert!(res.has_fatal_error());
    assert!(res.error_count() >= 1);

    // 2. Leading blank lines before TSV header auto-detect Tab delimiter
    let tsv = "\n  \nID\tParent\nID\tSC %\nex:Heart\tex:Organ\n";
    let good_spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res = evaluate(
        tsv,
        Delimiter::Auto,
        &good_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(res.delimiter, Delimiter::Tab);
    assert!(res.is_valid());

    // 3. Pattern mode with exactly max_rows + 1 returns TableError (no off-by-one)
    let p_csv = "cls\nex:A\nex:B\n";
    let p_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: vec![ColumnDeclaration {
                column: "cls".into(),
                kind: EntityKind::Class,
                split: None,
            }],
            rules: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let tight_rows = Limits {
        max_rows: 1,
        ..Limits::default()
    };
    let err = evaluate(p_csv, Delimiter::Comma, &p_spec, &ex_vocab(), &tight_rows).unwrap_err();
    assert_eq!(err.code, "too-many-rows");

    // 4. Annotation-only split column attaches split annotations to rule axiom
    let ann_csv = "cls,parent,sources\nex:Heart,ex:Organ,ref1|ref2\n";
    let mut split = BTreeMap::new();
    split.insert("sources".to_string(), "|".to_string());
    let ann_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: Vec::new(),
            rules: vec![PatternRule {
                name: Some("sub".into()),
                template: "${cls|iri} SubClassOf ${parent|iri}".into(),
                syntax: PatternSyntax::Manchester,
                skip_if_empty: true,
                required_columns: Vec::new(),
                split,
                annotations: vec![RuleAnnotation {
                    property: "dcterms:source".into(),
                    value: "${sources}".into(),
                    lang: None,
                    datatype: None,
                    iri: false,
                }],
            }],
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res = evaluate(
        ann_csv,
        Delimiter::Comma,
        &ann_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert!(res.is_valid(), "diagnostics: {:?}", res.diagnostics);
    assert_eq!(res.axioms.len(), 1);
    assert_eq!(res.axioms[0].axiom.annotations.len(), 2);

    // 5. Duplicate header referenced by name is diagnosed as ambiguous-column
    let dup_hdr = "cls,cls\nex:A,ex:B\n";
    let res = evaluate(
        dup_hdr,
        Delimiter::Comma,
        &p_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert!(!res.is_valid());
    assert!(res.diagnostics.iter().any(|d| d.code == "ambiguous-column"));
}
