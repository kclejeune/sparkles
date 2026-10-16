//! ShEx 2.2 `EXTENDS` and `ABSTRACT`: the behaviours the shexTest suite (run from
//! `shextest.rs`, vendored under `shex22/`) does not pin down, and the ones its schemas
//! exercise only in one form.

use oxrdf::{NamedNode, Term};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shex::{
    Association, NoImports, NodeSelector, Schema, ShapeLabel, ShapeMap, ShexFailure, Status,
    ValidateOptions,
};

const EX: &str = "http://ex.org/";

fn store(turtle: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let text = format!("@prefix ex: <{EX}> .\n{turtle}");
    s.load(&[Source::from_bytes(
        text.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn compile(text: &str) -> Result<sparkles_shex::CompiledSchema, String> {
    let s = Schema::parse_shexc(&format!("PREFIX ex: <{EX}>\n{text}"), None)
        .map_err(|e| e.to_string())?;
    sparkles_shex::compile(&s, &NoImports).map_err(|e| e.to_string())
}

/// Whether each `(node, shape)` pair conforms.
fn check(schema: &str, data: &str, pairs: &[(&str, &str)]) -> Vec<bool> {
    let store = store(data);
    let schema = compile(schema).unwrap();
    let map = ShapeMap(
        pairs
            .iter()
            .map(|(n, s)| Association {
                node: NodeSelector::Term(Term::from(NamedNode::new_unchecked(format!("{EX}{n}")))),
                shape: ShapeLabel::Iri(format!("{EX}{s}")),
            })
            .collect(),
    );
    let r = sparkles_shex::validate(
        &store.snapshot(),
        &schema,
        &map,
        &ValidateOptions::default(),
    )
    .unwrap();
    r.results
        .iter()
        .map(|x| x.status == Status::Conformant)
        .collect()
}

#[test]
fn a_shape_has_its_own_arcs_and_its_parents() {
    let schema = "ex:Person { ex:name . }
                  ex:Student EXTENDS @ex:Person { ex:school . }";
    let data = r#"
        ex:a ex:name "A" ; ex:school "S" .
        ex:b ex:school "S" .
        ex:c ex:name "C" .
        ex:d ex:name "D" ; ex:school "S" ; ex:other 1 ."#;
    assert_eq!(
        check(
            schema,
            data,
            &[
                ("a", "Student"),
                ("b", "Student"),
                ("c", "Student"),
                ("d", "Student"),
                ("c", "Person"),
            ]
        ),
        [true, false, false, true, true]
    );
}

#[test]
fn closed_applies_to_the_arcs_of_every_part() {
    let schema = "ex:Person { ex:name . }
                  ex:Student EXTENDS @ex:Person CLOSED { ex:school . }";
    let data = r#"
        ex:a ex:name "A" ; ex:school "S" .
        ex:d ex:name "D" ; ex:school "S" ; ex:other 1 ."#;
    assert_eq!(
        check(schema, data, &[("a", "Student"), ("d", "Student")]),
        [true, false]
    );
}

#[test]
fn a_shared_predicate_is_divided_between_the_parts() {
    // two ex:p arcs: one for the child, one for the parent
    let schema = "ex:Base { ex:p . }
                  ex:Child EXTENDS @ex:Base { ex:p . }";
    let data = "ex:one ex:p 1 . ex:two ex:p 1, 2 . ex:three ex:p 1, 2, 3 .";
    assert_eq!(
        check(
            schema,
            data,
            &[("one", "Child"), ("two", "Child"), ("three", "Child")]
        ),
        [false, true, false]
    );
}

#[test]
fn an_ancestors_other_conjuncts_see_only_its_arcs() {
    // Base's conjunct wants exactly two ex:p values from the arcs Base takes, so the
    // child's own ex:p arc must be the third
    let schema = "ex:Two { ex:p . {2} }
                  ex:Base @ex:Two AND { ex:p . + }
                  ex:Child EXTENDS @ex:Base { ex:p . }";
    let data = "ex:two ex:p 1, 2 . ex:three ex:p 1, 2, 3 . ex:four ex:p 1, 2, 3, 4 .";
    assert_eq!(
        check(
            schema,
            data,
            &[("two", "Child"), ("three", "Child"), ("four", "Child")]
        ),
        [false, true, false]
    );
}

#[test]
fn extra_of_the_child_lets_an_arc_stay_unmatched() {
    let schema = "ex:Base { ex:p [1] }
                  ex:Child EXTENDS @ex:Base EXTRA ex:p { ex:q . }";
    let data = "ex:a ex:p 1, 2 ; ex:q 0 . ex:b ex:p 2 ; ex:q 0 .";
    // a: the 1 is Base's, the 2 stays over; b: Base needs a 1
    assert_eq!(
        check(schema, data, &[("a", "Child"), ("b", "Child")]),
        [true, false]
    );
}

#[test]
fn inverse_constraints_are_parts_too() {
    let schema = "ex:Base { ^ex:knows . + }
                  ex:Child EXTENDS @ex:Base { ex:name . }";
    let data = r#"ex:x ex:knows ex:a . ex:a ex:name "A" . ex:b ex:name "B" ."#;
    assert_eq!(
        check(schema, data, &[("a", "Child"), ("b", "Child")]),
        [true, false]
    );
}

#[test]
fn references_reach_the_shapes_that_extend() {
    let schema = "ABSTRACT ex:Animal { ex:name . }
                  ex:Dog EXTENDS @ex:Animal { ex:barks . }
                  ex:Puppy EXTENDS @ex:Dog { ex:age . }
                  ex:Owner { ex:pet @ex:Animal }";
    let data = r#"
        ex:rex ex:name "Rex" ; ex:barks true .
        ex:pup ex:name "Pup" ; ex:barks true ; ex:age 1 .
        ex:cat ex:name "Tom" .
        ex:o1 ex:pet ex:rex . ex:o2 ex:pet ex:pup . ex:o3 ex:pet ex:cat ."#;
    assert_eq!(
        check(
            schema,
            data,
            &[
                ("o1", "Owner"),
                ("o2", "Owner"),
                ("o3", "Owner"),
                ("rex", "Animal"),
                ("pup", "Dog"),
                ("cat", "Animal"),
            ]
        ),
        [true, true, false, true, true, false]
    );
}

#[test]
fn a_shape_that_is_abstract_cannot_be_the_target_alone() {
    let schema = "ABSTRACT ex:A { ex:p . }";
    assert_eq!(
        check(schema, "ex:a ex:p 1 .", &[("a", "A")]),
        [false],
        "nothing extends it"
    );
}

#[test]
fn recursion_runs_through_extends() {
    let schema = "ex:Node { ex:next @ex:Node ? }
                  ex:Tagged EXTENDS @ex:Node { ex:tag . ; ex:next @ex:Tagged }";
    let data = r#"
        ex:a ex:tag 1 ; ex:next ex:b . ex:b ex:tag 2 ; ex:next ex:a .
        ex:c ex:tag 1 ; ex:next ex:d . ex:d ex:next ex:e . ex:e ex:next ex:d ."#;
    // the cycle a, b conforms; c reaches d, which has no tag
    assert_eq!(
        check(
            schema,
            data,
            &[("a", "Tagged"), ("b", "Tagged"), ("c", "Tagged")]
        ),
        [true, true, false]
    );
}

#[test]
fn a_diamond_counts_its_apex_once() {
    let schema = r#"ex:Top { ex:t . }
                    ex:Left EXTENDS @ex:Top { ex:l . }
                    ex:Right EXTENDS @ex:Top { ex:r . }
                    ex:Bottom EXTENDS @ex:Left EXTENDS @ex:Right { ex:b . }"#;
    let data = "ex:ok ex:t 1 ; ex:l 1 ; ex:r 1 ; ex:b 1 .
                ex:two ex:t 1, 2 ; ex:l 1 ; ex:r 1 ; ex:b 1 .
                ex:none ex:l 1 ; ex:r 1 ; ex:b 1 .";
    assert_eq!(
        check(
            schema,
            data,
            &[("ok", "Bottom"), ("two", "Bottom"), ("none", "Bottom")]
        ),
        [true, false, false]
    );
}

#[test]
fn a_parent_that_is_a_disjunction_of_shapes() {
    let schema = "ex:Base { ex:p . ; ex:q . } OR { ex:p . ; ex:r . }
                  ex:Child EXTENDS @ex:Base CLOSED { }";
    let data = "ex:pq ex:p 1 ; ex:q 1 . ex:pr ex:p 1 ; ex:r 1 . ex:pqr ex:p 1 ; ex:q 1 ; ex:r 1 .";
    assert_eq!(
        check(
            schema,
            data,
            &[("pq", "Child"), ("pr", "Child"), ("pqr", "Child")]
        ),
        [true, true, false]
    );
}

#[test]
fn failures_name_the_extension() {
    let store = store("ex:a ex:school 1 .");
    let schema = compile(
        "ex:Person { ex:name . }
         ex:Student EXTENDS @ex:Person { ex:school . }",
    )
    .unwrap();
    let map = ShapeMap(vec![Association {
        node: NodeSelector::Term(NamedNode::new_unchecked(format!("{EX}a")).into()),
        shape: ShapeLabel::Iri(format!("{EX}Student")),
    }]);
    let r = sparkles_shex::validate(
        &store.snapshot(),
        &schema,
        &map,
        &ValidateOptions::default(),
    )
    .unwrap();
    assert_eq!(r.results[0].status, Status::Nonconformant);
    assert!(matches!(
        r.results[0].failures[0],
        ShexFailure::NoMatch { .. }
    ));
}

#[test]
fn the_partition_budget_stops_a_search() {
    // eight arcs that each fit four parts
    let schema = "ex:A { ex:p . * } ex:B EXTENDS @ex:A { ex:p . * }
                  ex:C EXTENDS @ex:B { ex:p . * } ex:D EXTENDS @ex:C { ex:p . {9} }";
    let store = store("ex:a ex:p 1, 2, 3, 4, 5, 6, 7, 8 .");
    let schema = compile(schema).unwrap();
    let map = ShapeMap(vec![Association {
        node: NodeSelector::Term(NamedNode::new_unchecked(format!("{EX}a")).into()),
        shape: ShapeLabel::Iri(format!("{EX}D")),
    }]);
    let opts = ValidateOptions {
        max_partitions: Some(100),
        ..Default::default()
    };
    let r = sparkles_shex::validate(&store.snapshot(), &schema, &map, &opts);
    assert!(r.is_err(), "{r:?}");
}

#[test]
fn schema_checks() {
    let err = |s: &str| compile(s).expect_err(s);
    assert!(err("ex:S EXTENDS @ex:S { ex:p . }").contains("extension cycle"));
    assert!(err("ex:S EXTENDS @ex:T { }\nex:T EXTENDS @ex:S { }").contains("extension cycle"));
    assert!(err("ex:S EXTENDS @ex:Missing { }").contains("undefined shape"));
    assert!(
        err("ABSTRACT ex:A { ex:p . }\nex:B { ex:q @ex:A }").contains("abstract"),
        "a reference to an abstract shape nothing concrete extends"
    );
    assert!(err("ex:N IRI\nex:S EXTENDS @ex:N { }").contains("extended"));
    // a reference to an abstract shape with a concrete descendant is fine
    compile("ABSTRACT ex:A { ex:p . }\nex:B EXTENDS @ex:A { }\nex:C { ex:q @ex:A }").unwrap();
}

/// An EXTRA arc whose value's typing is still unknown may only stay unmatched if it
/// does not match, so leaving it over must not be read as a sure success.
#[test]
fn an_undecided_extra_arc_is_not_left_over_for_sure() {
    let schema = "ex:T { ex:r . }
                  ex:Base { ex:p @ex:T ? }
                  ex:Child EXTENDS @ex:Base EXTRA ex:p { ex:q . }";
    let data = r#"
        ex:n ex:p ex:v1, ex:v2 ; ex:q 1 .
        ex:v1 ex:r 1 .
        ex:v2 ex:r 2 ."#;
    // both p values conform to T, Base takes at most one, and a matching arc cannot
    // be left over even with EXTRA
    assert_eq!(check(schema, data, &[("n", "Child")]), [false]);
    // the same shape without a reference (the typing is never unknown) already fails
    let plain = "ex:Base { ex:p . ? }
                 ex:Child EXTENDS @ex:Base EXTRA ex:p { ex:q . }";
    assert_eq!(check(plain, data, &[("n", "Child")]), [false]);
}

/// The partition search recursed once per arc with a choice; a node with
/// tens of thousands of such arcs overflowed the stack and aborted the process.
#[test]
fn many_arcs_with_a_choice_do_not_overflow_the_stack() {
    let schema = "ex:A { ex:p . * }
                  ex:B EXTENDS @ex:A { ex:p . * }";
    let mut data = String::new();
    for i in 0..50_000 {
        data.push_str(&format!("ex:n ex:p {i} .\n"));
    }
    // run on a thread with the default 2 MiB stack, as a server worker does
    let r = std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || check(schema, &data, &[("n", "B")]))
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(r, [true]);
}

/// Every branch of this search hits a cap before a leaf, so a budget charged only at
/// leaves never stops it (about C(51,25) paths). It must end with the validation-work
/// budget error.
#[test]
fn a_search_without_leaves_is_bounded() {
    let schema = compile("ex:A { ex:p . {25} } ex:B EXTENDS @ex:A { ex:p . {25} }").unwrap();
    let mut data = String::new();
    for i in 0..51 {
        data.push_str(&format!("ex:n ex:p {i} .\n"));
    }
    let store = store(&data);
    let map = ShapeMap(vec![Association {
        node: NodeSelector::Term(Term::from(NamedNode::new_unchecked(format!("{EX}n")))),
        shape: ShapeLabel::Iri(format!("{EX}B")),
    }]);
    let opts = ValidateOptions {
        max_partitions: Some(1000),
        timeout: Some(std::time::Duration::from_secs(5)),
        ..ValidateOptions::default()
    };
    // a watchdog: before the fix the search runs for hours and ignores the timeout
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = sparkles_shex::validate(&store.snapshot(), &schema, &map, &opts)
            .map(|r| r.results[0].status == Status::Conformant);
        let _ = tx.send(r.map_err(|e| e.to_string()));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(conformant)) => assert!(!conformant, "51 arcs cannot match 25 + 25"),
        Ok(Err(_budget_or_timeout)) => {}
        Err(_) => panic!("the partition search ran past its timeout and budget"),
    }
}
