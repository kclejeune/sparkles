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

#[test]
fn axiom_triple_count_matches_writer_across_all_kinds() {
    use crate::write::{axiom_triple_count, axiom_triples, counter};

    let c1 = Class::new("http://ex.org/A");
    let c2 = Class::new("http://ex.org/B");
    let c3 = Class::new("http://ex.org/C");
    let op = ObjectProperty::new("http://ex.org/p");
    let op2 = ObjectProperty::new("http://ex.org/q");
    let dp = DataProperty::new("http://ex.org/d");
    let ap = AnnotationProperty::new("http://ex.org/ann");
    let ind1 = Individual::Named(NamedIndividual::new("http://ex.org/i1"));
    let ind2 = Individual::Named(NamedIndividual::new("http://ex.org/i2"));
    let ind3 = Individual::Named(NamedIndividual::new("http://ex.org/i3"));

    let nested_ann = Annotation {
        property: ap.clone(),
        value: AnnotationValue::Literal(Literal::simple("outer")),
        annotations: [Annotation {
            property: ap.clone(),
            value: AnnotationValue::Literal(Literal::simple("inner")),
            annotations: BTreeSet::new(),
        }]
        .into_iter()
        .collect(),
    };

    let complex_ce = ClassExpression::ObjectIntersectionOf(
        [
            ClassExpression::Class(c1.clone()),
            ClassExpression::some(op.clone(), ClassExpression::Class(c2.clone())),
            ClassExpression::ObjectMinCardinality(
                2,
                ObjectPropertyExpression::InverseOf(op2.clone()),
                Some(Box::new(ClassExpression::Class(c3.clone()))),
            ),
            ClassExpression::DataSomeValuesFrom(
                vec![dp.clone()],
                DataRange::DatatypeRestriction(
                    Datatype::new("http://www.w3.org/2001/XMLSchema#integer"),
                    [FacetRestriction {
                        facet: Iri::new_unchecked("http://www.w3.org/2001/XMLSchema#minInclusive"),
                        value: Literal::typed("1", "http://www.w3.org/2001/XMLSchema#integer"),
                    }]
                    .into_iter()
                    .collect(),
                ),
            ),
        ]
        .into_iter()
        .collect(),
    );

    let mut axioms = vec![
        Axiom::declaration(Entity::Class(c1.clone())),
        Axiom::sub_class_of(c1.clone(), complex_ce.clone()),
        Axiom::equivalent_classes([
            ClassExpression::Class(c1.clone()),
            ClassExpression::Class(c2.clone()),
            ClassExpression::Class(c3.clone()),
        ]),
        Axiom::disjoint_classes([
            ClassExpression::Class(c1.clone()),
            ClassExpression::Class(c2.clone()),
        ]),
        Axiom::disjoint_classes([
            ClassExpression::Class(c1.clone()),
            ClassExpression::Class(c2.clone()),
            ClassExpression::Class(c3.clone()),
        ]),
        Axiom::new(AxiomKind::DisjointUnion(
            c1.clone(),
            [
                ClassExpression::Class(c2.clone()),
                ClassExpression::Class(c3.clone()),
            ]
            .into_iter()
            .collect(),
        )),
        Axiom::new(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![
                ObjectPropertyExpression::Property(op.clone()),
                ObjectPropertyExpression::InverseOf(op2.clone()),
            ]),
            ObjectPropertyExpression::Property(op.clone()),
        )),
        Axiom::new(AxiomKind::HasKey(
            ClassExpression::Class(c1.clone()),
            [ObjectPropertyExpression::Property(op.clone())]
                .into_iter()
                .collect(),
            [dp.clone()].into_iter().collect(),
        )),
        Axiom::new(AxiomKind::SameIndividual(
            [ind1.clone(), ind2.clone(), ind3.clone()]
                .into_iter()
                .collect(),
        )),
        Axiom::new(AxiomKind::DifferentIndividuals(
            [ind1.clone(), ind2.clone()].into_iter().collect(),
        )),
        Axiom::new(AxiomKind::DifferentIndividuals(
            [ind1.clone(), ind2.clone(), ind3.clone()]
                .into_iter()
                .collect(),
        )),
        Axiom::class_assertion(complex_ce, ind1.clone()),
        Axiom::new(AxiomKind::ObjectPropertyAssertion(
            ObjectPropertyExpression::InverseOf(op.clone()),
            ind1.clone(),
            ind2.clone(),
        )),
        Axiom::new(AxiomKind::NegativeObjectPropertyAssertion(
            ObjectPropertyExpression::Property(op.clone()),
            ind1.clone(),
            ind2.clone(),
        )),
        Axiom::new(AxiomKind::DataPropertyAssertion(
            dp.clone(),
            ind1.clone(),
            Literal::simple("v"),
        )),
        Axiom::new(AxiomKind::NegativeDataPropertyAssertion(
            dp,
            ind1,
            Literal::simple("v"),
        )),
        Axiom::new(AxiomKind::AnnotationAssertion(
            ap,
            AnnotationSubject::Iri(c1.0.clone()),
            AnnotationValue::Literal(Literal::simple("lbl")),
        )),
    ];

    // Check both unannotated and nested-annotated variants of every axiom.
    let annotated: Vec<Axiom> = axioms
        .iter()
        .map(|a| {
            let mut copy = a.clone();
            copy.annotations.insert(nested_ann.clone());
            copy
        })
        .collect();
    axioms.extend(annotated);

    for ax in &axioms {
        let mut fresh = counter("t");
        let actual = axiom_triples(ax, &mut fresh).len();
        let counted = axiom_triple_count(ax);
        assert_eq!(counted, actual, "mismatch for {ax:?}");
    }
}

#[test]
fn resolved_iri_vocab_ofn_prefix_and_replicated_annotation_bytes_are_charged() {
    // 1. Long namespace with short ROBOT IDs triggers expansion-bytes-limit in Pass 1.
    let long_ns = format!("http://example.org/{}/", "a".repeat(1_500));
    let mut prefixes = BTreeMap::new();
    prefixes.insert("p".to_string(), long_ns.clone());
    let csv = "ID\nID\np:1\np:2\np:3\np:4\n";
    let spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: prefixes.clone(),
    };
    let tight = Limits {
        max_total_expanded_bytes: 6_000,
        ..Limits::default()
    };
    let res = evaluate(csv, Delimiter::Comma, &spec, &ex_vocab(), &tight).unwrap();
    assert!(
        res.budget_exceeded,
        "expected Pass 1 IRI/vocab budget hit: {res:?}"
    );
    assert!(res.has_fatal_error());

    // 2. ROBOT >A replicated annotation bytes charged BEFORE cloning into split targets.
    let long_ann = "x".repeat(400);
    let csv_rep = format!(
        "ID,Parents,Note\nID,SC % SPLIT=|,>A rdfs:comment\nex:Heart,ex:A|ex:B|ex:C|ex:D|ex:E|ex:F|ex:G|ex:H|ex:I|ex:J,{long_ann}\n"
    );
    let rep_spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let tight_rep = Limits {
        max_total_expanded_bytes: 2_000,
        ..Limits::default()
    };
    let res_rep = evaluate(
        &csv_rep,
        Delimiter::Comma,
        &rep_spec,
        &ex_vocab(),
        &tight_rep,
    )
    .unwrap();
    assert!(
        res_rep.budget_exceeded,
        "expected replicated >A annotation bytes budget hit: {res_rep:?}"
    );
    assert!(res_rep.axioms.is_empty());

    // 3. OFN prefixed terms expand long namespaces and are charged BEFORE parsing.
    let ofn_csv = "a,b\n1,2\n3,4\n5,6\n";
    let ofn_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: Vec::new(),
            rules: vec![PatternRule::functional("SubClassOf(p:${a} p:${b})")],
        },
        base_iri: None,
        prefixes,
    };
    let res_ofn = evaluate(ofn_csv, Delimiter::Comma, &ofn_spec, &ex_vocab(), &tight).unwrap();
    assert!(
        res_ofn.budget_exceeded,
        "expected OFN prefix expansion budget hit: {res_ofn:?}"
    );

    // 4. Long diagnostic column/rule name is truncated to MAX_DIAGNOSTIC_COLUMN_BYTES.
    let huge_col = "C".repeat(1_000);
    let col_csv = format!("ID,{huge_col}\nID,SC %\nex:A,not valid (\n");
    let res_col = evaluate(
        &col_csv,
        Delimiter::Comma,
        &rep_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    let diag = res_col
        .diagnostics
        .iter()
        .find(|d| d.code == "invalid-expression")
        .expect("diagnostic present");
    assert!(
        diag.column.as_deref().unwrap().len() <= MAX_DIAGNOSTIC_COLUMN_BYTES,
        "diagnostic column was not truncated: {}",
        diag.column.as_deref().unwrap().len()
    );
}

#[test]
fn ofn_inline_annotations_enforce_max_annotations_per_axiom() {
    let csv = "c,p\nex:Heart,ex:Organ\n";
    let ofn_template = "SubClassOf(Annotation(rdfs:comment \"a\") Annotation(rdfs:comment \"b\") Annotation(rdfs:comment \"c\") ${c} ${p})";
    let spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: Vec::new(),
            rules: vec![PatternRule::functional(ofn_template)],
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let tight = Limits {
        max_annotations_per_axiom: 2,
        ..Limits::default()
    };
    let res = evaluate(csv, Delimiter::Comma, &spec, &ex_vocab(), &tight).unwrap();
    assert!(!res.is_valid());
    assert!(res.axioms.is_empty());
    assert!(
        res.diagnostics.iter().any(|d| d.code == "annotation-limit"),
        "expected annotation-limit diagnostic: {:?}",
        res.diagnostics
    );
}

#[test]
fn failed_row_vocab_and_truncated_splits_are_excluded_from_retained_rows() {
    let robot_spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };

    // 1. Alias supplied ONLY by a failing row must NOT resolve for another row.
    let alias_csv = "\
ID,Label,Parent,Source
ID,LABEL,SC %,>AI dcterms:source
ex:BadClass,Bad Alias,ex:Organ,unknownprefix:BadRef
ex:GoodClass,Good,'Bad Alias',ex:Ref1
";
    let res = evaluate(
        alias_csv,
        Delimiter::Comma,
        &robot_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert!(!res.is_valid());
    let r = Renderer::new([("ex", "http://ex.org/")]);
    let ofns: Vec<String> = res.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();
    assert!(
        !ofns.iter().any(|s| s.contains("ex:BadClass")),
        "failed row entity leaked via alias into retained axioms: {ofns:?}"
    );
    assert!(
        !ofns.iter().any(|s| s.contains("ex:GoodClass")),
        "dependent row should fail once failed row alias is excluded: {ofns:?}"
    );

    // 2. Failing row whose duplicate label made an existing label ambiguous must NOT drop a good row.
    let mut vocab = ex_vocab();
    vocab.add_label("http://ex.org/Organ", "Unique Organ");
    let ambig_csv = "\
ID,Label,Parent,Source
ID,LABEL,SC %,>AI dcterms:source
ex:BadDup,Unique Organ,ex:Organ,unknownprefix:BadRef
ex:Heart,Heart,'Unique Organ',ex:Ref1
";
    let res_ambig = evaluate(
        ambig_csv,
        Delimiter::Comma,
        &robot_spec,
        &vocab,
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(res_ambig.rows_with_errors, 1, "{:?}", res_ambig.diagnostics);
    let ofns_ambig: Vec<String> = res_ambig.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();
    assert!(
        ofns_ambig
            .iter()
            .any(|s| s.contains("SubClassOf") && s.contains("ex:Heart") && s.contains("ex:Organ")),
        "good row should survive after failing duplicate-label row is excluded: {ofns_ambig:?}"
    );
    assert!(
        !ofns_ambig.iter().any(|s| s.contains("ex:BadDup")),
        "failing duplicate row must not emit axioms: {ofns_ambig:?}"
    );

    // 3. Truncated LABEL split must NOT register its first tokens in Pass 1 vocab.
    let trunc_csv = "\
ID,Label,Parent
ID,LABEL SPLIT=|,SC %
ex:TruncRow,L1|L2|L3,ex:Organ
ex:Other,Other,'L1'
";
    let tight_split = Limits {
        max_split_values_per_cell: 2,
        ..Limits::default()
    };
    let res_trunc = evaluate(
        trunc_csv,
        Delimiter::Comma,
        &robot_spec,
        &ex_vocab(),
        &tight_split,
    )
    .unwrap();
    let ofns_trunc: Vec<String> = res_trunc.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();
    assert!(
        !ofns_trunc.iter().any(|s| s.contains("ex:TruncRow")),
        "truncated LABEL split leaked into vocab: {ofns_trunc:?}"
    );
    assert!(
        res_trunc.axioms.is_empty(),
        "dependent row using truncated LABEL token must fail: {ofns_trunc:?}"
    );

    // 4. Pattern mode: failed row declaration & truncated declaration split must not enter vocab.
    let pat_csv = "cls,parent\nex:BadDecl|ex:Second|ex:Third,ex:Organ\nex:Good,BadDecl\n";
    let pat_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: vec![ColumnDeclaration {
                column: "cls".into(),
                kind: EntityKind::Class,
                split: Some("|".into()),
            }],
            rules: vec![PatternRule::manchester("${cls} SubClassOf ${parent}")],
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res_pat = evaluate(
        pat_csv,
        Delimiter::Comma,
        &pat_spec,
        &ex_vocab(),
        &tight_split,
    )
    .unwrap();
    assert!(
        res_pat.axioms.is_empty(),
        "truncated Pattern declaration leaked into vocab: {:?}",
        res_pat.axioms
    );
}

#[test]
fn malformed_prefixes_and_qualified_names_never_fall_through_to_base_iri() {
    // 1. Non-ASCII prefix key in spec.prefixes is rejected structurally across modes.
    let mut bad_prefixes = BTreeMap::new();
    bad_prefixes.insert("é".to_string(), "http://ex.org/".to_string());
    let bad_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: Vec::new(),
            rules: vec![PatternRule::functional("SubClassOf(é:${a} ex:Organ)")],
        },
        base_iri: Some("http://base.org/".to_string()),
        prefixes: bad_prefixes,
    };
    let res = evaluate(
        "a\nHeart\n",
        Delimiter::Comma,
        &bad_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert!(res.has_fatal_error());
    assert!(
        res.diagnostics.iter().any(|d| d.code == "invalid-prefix"),
        "expected invalid-prefix diagnostic: {:?}",
        res.diagnostics
    );

    // 2. Malformed qualified name (non-ASCII or invalid prefix) with baseIri configured
    //    must fail instead of resolving as http://base.org/é:Heart.
    let robot_spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: Some("http://base.org/".to_string()),
        prefixes: BTreeMap::new(),
    };
    let csv = "ID,Parent\nID,SC %\né:Heart,ex:Organ\nGoodLocal,ex:Organ\n";
    let res2 = evaluate(
        csv,
        Delimiter::Comma,
        &robot_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(res2.rows_with_errors, 1, "{:?}", res2.diagnostics);
    let r = Renderer::new([("base", "http://base.org/"), ("ex", "http://ex.org/")]);
    let ofns: Vec<String> = res2.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();
    assert!(
        ofns.iter().any(|s| s.contains("base:GoodLocal")),
        "unqualified bare local name should still resolve under baseIri: {ofns:?}"
    );
    assert!(
        !ofns.iter().any(|s| s.contains("é:Heart")),
        "malformed qualified name must not fall through to baseIri: {ofns:?}"
    );
}

#[test]
fn tight_budgets_catch_repeated_robot_subject_annotation_prop_and_data_prop_literal() {
    // 1. Repeated ROBOT subject cloning across split values is charged before allocation.
    let long_subj = format!("http://ex.org/{}", "S".repeat(600));
    let csv_subj = format!(
        "ID,Parents\nID,SC % SPLIT=|\n<{long_subj}>,ex:A|ex:B|ex:C|ex:D|ex:E|ex:F|ex:G|ex:H\n"
    );
    let robot_spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let tight_subj = Limits {
        max_total_expanded_bytes: 3_000,
        ..Limits::default()
    };
    let res_subj = evaluate(
        &csv_subj,
        Delimiter::Comma,
        &robot_spec,
        &ex_vocab(),
        &tight_subj,
    )
    .unwrap();
    assert!(
        res_subj.budget_exceeded,
        "repeated ROBOT subject clones must hit expansion-bytes-limit: {res_subj:?}"
    );
    assert!(res_subj.axioms.is_empty());

    // 2. ROBOT `>A` per-value property cloning is charged before building annotations.
    let long_prop = format!("http://ex.org/{}", "P".repeat(700));
    let csv_prop = format!(
        "ID,Parent,Notes\nID,SC %,>A <{long_prop}> SPLIT=|\nex:Heart,ex:Organ,a|b|c|d|e|f|g|h\n"
    );
    let tight_prop = Limits {
        max_total_expanded_bytes: 3_500,
        ..Limits::default()
    };
    let res_prop = evaluate(
        &csv_prop,
        Delimiter::Comma,
        &robot_spec,
        &ex_vocab(),
        &tight_prop,
    )
    .unwrap();
    assert!(
        res_prop.budget_exceeded,
        "per-value >A property clones must hit expansion-bytes-limit: {res_prop:?}"
    );
    assert!(res_prop.axioms.is_empty());

    // 3. ROBOT `I <data-prop>` typed literal datatype prefix expansion is charged before parsing.
    let mut vocab = ex_vocab();
    vocab.add_entity(Entity::DataProperty(DataProperty::new(
        "http://ex.org/hasVal",
    )));
    let mut prefixes = BTreeMap::new();
    prefixes.insert(
        "p".to_string(),
        format!("http://ex.org/{}/", "d".repeat(600)),
    );
    let dp_spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes,
    };
    let csv_dp = "\
ID,Val
ID,I ex:hasVal
ex:i1,\"\"\"1\"\"^^p:t\"
ex:i2,\"\"\"2\"\"^^p:t\"
ex:i3,\"\"\"3\"\"^^p:t\"
ex:i4,\"\"\"4\"\"^^p:t\"
";
    let tight_dp = Limits {
        max_total_expanded_bytes: 1_500,
        ..Limits::default()
    };
    let res_dp = evaluate(csv_dp, Delimiter::Comma, &dp_spec, &vocab, &tight_dp).unwrap();
    assert!(
        res_dp.budget_exceeded,
        "ROBOT I <data-prop> typed literal prefix expansion must hit budget: {res_dp:?}"
    );
    assert!(res_dp.has_fatal_error());
    assert!(
        res_dp.axioms.len() < 8,
        "budget must stop evaluation before all 4 rows emit axioms: {:?}",
        res_dp.axioms
    );

    // 4. Spec prefix namespace longer than max_expanded_bytes_per_template is rejected up front.
    let mut huge_prefixes = BTreeMap::new();
    huge_prefixes.insert(
        "p".to_string(),
        format!("http://ex.org/{}/", "x".repeat(2_000)),
    );
    let huge_ns_spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: huge_prefixes,
    };
    let tight_ns = Limits {
        max_expanded_bytes_per_template: 512,
        ..Limits::default()
    };
    let res_ns = evaluate(
        "ID,Label\nID,LABEL\nex:A,A\n",
        Delimiter::Comma,
        &huge_ns_spec,
        &ex_vocab(),
        &tight_ns,
    )
    .unwrap();
    assert!(
        res_ns.budget_exceeded,
        "oversized prefix namespace must be refused: {res_ns:?}"
    );
}

#[test]
fn tight_budgets_catch_ofn_comments_apostrophes_escapes_and_manchester_vocab_lookups() {
    let mut prefixes = BTreeMap::new();
    prefixes.insert(
        "p".to_string(),
        format!("http://ex.org/{}/", "n".repeat(400)),
    );
    let tight = Limits {
        max_total_expanded_bytes: 1_200,
        ..Limits::default()
    };

    // 1. OFN `# "` and `# '` line comments and `'` inside PN_LOCAL cannot bypass charging.
    for tmpl in [
        "# \"\nSubClassOf(p:${a} ObjectIntersectionOf(p:1 p:2 p:3 p:4))",
        "# '\nSubClassOf(p:${a} ObjectIntersectionOf(p:1 p:2 p:3 p:4))",
        "SubClassOf(p:a' ObjectIntersectionOf(p:${a} p:1 p:2 p:3 p:4))",
        "SubClassOf(p:a\\-b ObjectIntersectionOf(p:${a} p:1 p:2 p:3 p:4))",
    ] {
        let spec = PatternSpec {
            mode: TemplateMode::Pattern {
                declarations: Vec::new(),
                rules: vec![PatternRule::functional(tmpl)],
            },
            base_iri: None,
            prefixes: prefixes.clone(),
        };
        let res = evaluate("a\nx\n", Delimiter::Comma, &spec, &ex_vocab(), &tight).unwrap();
        assert!(
            res.budget_exceeded,
            "OFN template {tmpl:?} bypassed byte budget: {res:?}"
        );
        assert!(res.axioms.is_empty());
    }

    // 2. Manchester vocabulary lookups (quoted label, bare local name, empty-prefix fallback)
    //    resolving short tokens to long IRIs must be charged before allocation.
    let long_iri_a = format!("http://ex.org/{}/LongClassA", "a".repeat(500));
    let long_iri_b = format!("http://ex.org/{}/LongClassB", "b".repeat(500));
    let long_empty_ns = format!("http://ex.org/{}/", "e".repeat(500));
    let mut vocab = Vocabulary::new([("", long_empty_ns.as_str())]);
    vocab.add_entity(Entity::Class(Class::new(&long_iri_a)));
    vocab.add_label(&long_iri_a, "short a");
    vocab.add_entity(Entity::Class(Class::new(&long_iri_b)));
    vocab.add_entity(Entity::Class(Class::new(format!("{long_empty_ns}C1"))));

    // 2a. Quoted label resolving to long IRI repeatedly.
    let quoted_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: Vec::new(),
            rules: vec![PatternRule::manchester(
                "${x} SubClassOf 'short a' and 'short a' and 'short a'",
            )],
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res_quoted = evaluate(
        "x\n'short a'\n",
        Delimiter::Comma,
        &quoted_spec,
        &vocab,
        &tight,
    )
    .unwrap();
    assert!(
        res_quoted.budget_exceeded,
        "quoted Manchester label resolution must hit budget: {res_quoted:?}"
    );

    // 2b. Bare local name resolving to long IRI repeatedly.
    let bare_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: Vec::new(),
            rules: vec![PatternRule::manchester(
                "LongClassB SubClassOf LongClassB and LongClassB and LongClassB",
            )],
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res_bare = evaluate("x\n1\n", Delimiter::Comma, &bare_spec, &vocab, &tight).unwrap();
    assert!(
        res_bare.budget_exceeded,
        "bare Manchester local-name resolution must hit budget: {res_bare:?}"
    );

    // 2c. Empty-prefix fallback resolving bare word `C1` against `""` namespace.
    let empty_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: Vec::new(),
            rules: vec![PatternRule::manchester("C1 SubClassOf C1 and C1 and C1")],
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res_empty = evaluate("x\n1\n", Delimiter::Comma, &empty_spec, &vocab, &tight).unwrap();
    assert!(
        res_empty.budget_exceeded,
        "empty-prefix Manchester fallback must hit budget: {res_empty:?}"
    );
}

#[test]
fn diagnostic_strings_bound_capacity_and_structural_staging_count() {
    let huge_header = "H".repeat(20_000);
    // 1. ROBOT missing-ID path with a huge ID header bounds both len and capacity.
    let csv = format!("{huge_header},Parent\nID,SC %\n,ex:Organ\n,ex:Organ\n");
    let robot_spec = PatternSpec {
        mode: TemplateMode::Robot {
            header_template_row: true,
            columns: Vec::new(),
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let res = evaluate(
        &csv,
        Delimiter::Comma,
        &robot_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(res.rows_with_errors, 2);
    for d in &res.diagnostics {
        if let Some(col) = &d.column {
            assert!(col.len() <= MAX_DIAGNOSTIC_COLUMN_BYTES);
            assert!(
                col.capacity() <= MAX_DIAGNOSTIC_COLUMN_BYTES + 64,
                "retained diagnostic column capacity too large: {}",
                col.capacity()
            );
        }
        assert!(d.message.capacity() <= Limits::default().max_diagnostic_message_bytes + 64);
    }

    // 2. Pattern mode with huge rule name and many unknown columns enforces max_diagnostics
    //    and bounds staged structural diagnostic capacity.
    let huge_rule_name = "R".repeat(20_000);
    let mut rules = Vec::new();
    for i in 0..10 {
        let mut r = PatternRule::manchester(format!("${{missing_{i}}} SubClassOf ex:Organ"));
        r.name = Some(huge_rule_name.clone());
        rules.push(r);
    }
    let pat_spec = PatternSpec {
        mode: TemplateMode::Pattern {
            declarations: Vec::new(),
            rules,
        },
        base_iri: None,
        prefixes: BTreeMap::new(),
    };
    let tight_diags = Limits {
        max_diagnostics: 3,
        max_diagnostic_message_bytes: 200,
        ..Limits::default()
    };
    let res_pat = evaluate(
        "a\n1\n",
        Delimiter::Comma,
        &pat_spec,
        &ex_vocab(),
        &tight_diags,
    )
    .unwrap();
    assert!(res_pat.diagnostics_truncated);
    assert_eq!(res_pat.diagnostics.len(), 3);
    assert_eq!(res_pat.structural_error_count, 10);
    for d in &res_pat.diagnostics {
        if let Some(col) = &d.column {
            assert!(col.len() <= MAX_DIAGNOSTIC_COLUMN_BYTES);
            assert!(col.capacity() <= MAX_DIAGNOSTIC_COLUMN_BYTES + 64);
        }
        assert!(d.message.len() <= 200);
        assert!(d.message.capacity() <= 264);
    }
}

#[test]
fn mutually_dependent_rows_recover_when_failed_row_introduced_ambiguity() {
    // Row A declares Alpha and uses Beta; Row B declares Beta and uses Alpha;
    // Row C also declares Beta and fails independently on an invalid annotation IRI.
    // Under skip, removing C must let BOTH A and B recover cleanly together.
    let csv = "\
ID,Label,Parent,Source
ID,LABEL,SC %,>AI dcterms:source
ex:RowA,Alpha,'Beta',ex:RefA
ex:RowB,Beta,'Alpha',ex:RefB
ex:RowC,Beta,ex:Organ,unknownprefix:BadRef
";
    let robot_spec = PatternSpec {
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
        &robot_spec,
        &ex_vocab(),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        res.rows_with_errors, 1,
        "only failing RowC should have errors; got {:?}",
        res.diagnostics
    );
    assert!(!res.budget_exceeded);
    let r = Renderer::new([("ex", "http://ex.org/")]);
    let ofns: Vec<String> = res.axioms.iter().map(|g| r.axiom(&g.axiom)).collect();
    assert!(
        ofns.iter()
            .any(|s| s.contains("SubClassOf") && s.contains("ex:RowA") && s.contains("ex:RowB")),
        "RowA SubClassOf RowB should recover: {ofns:?}"
    );
    assert!(
        ofns.iter()
            .any(|s| s.contains("SubClassOf") && s.contains("ex:RowB") && s.contains("ex:RowA")),
        "RowB SubClassOf RowA should recover: {ofns:?}"
    );
    assert!(
        !ofns.iter().any(|s| s.contains("ex:RowC")),
        "failing RowC must remain excluded: {ofns:?}"
    );
}

#[test]
fn bounded_renderers_stop_early_for_long_labels_and_prefixes() {
    let mut vocab = Vocabulary::new([("ex", "http://ex.org/")]);
    let big_label = "L".repeat(10_000);
    vocab.add_entity(Entity::Class(Class::new("http://ex.org/A")));
    vocab.add_label("http://ex.org/A", &big_label);
    vocab.add_entity(Entity::Class(Class::new("http://ex.org/B")));

    let ax = Axiom::sub_class_of(Class::new("http://ex.org/A"), Class::new("http://ex.org/B"));
    let manch_r = crate::manchester::Renderer::of_vocabulary(&vocab);
    assert!(
        manch_r.axiom_bounded(&ax, 256).is_err(),
        "Manchester bounded renderer must bail out when label exceeds limit"
    );
    assert_eq!(
        manch_r.axiom_bounded(&ax, 20_000).unwrap(),
        manch_r.axiom(&ax)
    );

    let ofn_r = Renderer::new([("ex", "http://ex.org/")]);
    assert_eq!(ofn_r.axiom_bounded(&ax, 256), Some(ofn_r.axiom(&ax)));
    assert_eq!(ofn_r.axiom_bounded(&ax, 8), None);
    assert!(crate::json::axiom_fits_bytes(&ax, 512));
    assert!(!crate::json::axiom_fits_bytes(&ax, 20));
}
