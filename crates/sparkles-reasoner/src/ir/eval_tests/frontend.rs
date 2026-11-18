//! The rule frontend (item 839's plan, §22.3): a rule text with relations compiles to a
//! program whose Jena rules are the Jena compiler's.

use super::*;
use crate::engine::frontend;
use crate::parser::parse_rules;

fn terms() -> Terms {
    let s = Store::in_memory(StoreOptions::default());
    Terms::new(s.snapshot())
}

/// A Jena rule inside a program is the IR of what `engine::compile` makes of it, with its
/// variable names, and the program's rules carry their lines and parsed indices.
#[test]
fn jena_rules_compile_as_the_jena_compiler_does() {
    let text = "@prefix ex: <http://ex.org/> .
.decl r(x)
[j: (?a ex:p ?b), notEqual(?a, ?b) -> (?b ex:q ?a)]
r(?x) :- (?x ex:q ?y).
[k: (?a ex:s ?b) -> [(?b ex:t ?a) <- (?a ex:u ?b)]]
";
    let rules = parse_rules(text).unwrap();
    let t = terms();
    let mut w = WorkingWarnings::new(&t.budget());
    let info = frontend::compile(&rules, &t, &mut w, false).unwrap();
    let jena = parse_rules(
        "@prefix ex: <http://ex.org/> .
[j: (?a ex:p ?b), notEqual(?a, ?b) -> (?b ex:q ?a)]",
    )
    .unwrap();
    let compiled = engine::compile_info(&jena, &t, &mut w, false).unwrap();
    let mut want = Rule::from_crule(&compiled.rules()[0]);
    want.vars = compiled.infos()[0].vars.clone();
    let mut got = info.program.rules[0].clone();
    for l in &mut got.body {
        if let Literal::Builtin(b) = l {
            b.origin = Some(0);
        }
    }
    assert_eq!(got, want);
    assert_eq!(info.program.rules[0].vars, vec!["a", "b"]);
    // the nested backward rule is skipped by a materialization, as Jena's compiler does
    assert_eq!(info.program.rules.len(), 3);
    assert_eq!(info.lines, vec![(3, 0), (4, 1), (5, 2)]);
}

/// Two identical builtin calls in different rules keep different occurrences, which
/// analysis witnesses and explanations name.
#[test]
fn builtin_occurrences_are_distinct() {
    let text = "@prefix ex: <http://ex.org/> .
.decl r(x, n)
r(?x, ?n) :- (?x ex:p ?y), makeTemp(?n).
r(?x, ?n) :- (?x ex:p ?y), makeTemp(?n).
[j: (?x ex:q ?y), makeTemp(?n) -> (?x ex:t ?n)]
";
    let rules = parse_rules(text).unwrap();
    let t = terms();
    let mut w = WorkingWarnings::new(&t.budget());
    let info = frontend::compile(&rules, &t, &mut w, false).unwrap();
    let origins: Vec<Option<usize>> = info
        .program
        .rules
        .iter()
        .flat_map(|r| r.body.iter())
        .filter_map(|l| match l {
            Literal::Builtin(b) => Some(b.origin),
            _ => None,
        })
        .collect();
    assert_eq!(origins, vec![Some(0), Some(1), Some(2)]);
}

/// The stratified `noValue` warning names the rule set an `@include` brought the rule
/// from, and a relation head variable that the body does not bind is an existential,
/// which the run refuses.
#[test]
fn included_rules_and_existentials() {
    let text = "@prefix ex: <http://ex.org/> .
@include <owl-rl> .
.decl r(x)
r(?x) :- (?x ex:p ?y).
(?x ex:q ex:none) :- (?x a ex:T), not r(?x).
";
    let rules = parse_rules(text).unwrap();
    let t = terms();
    let mut w = WorkingWarnings::new(&t.budget());
    match frontend::compile(&rules, &t, &mut w, false) {
        Ok(info) => {
            let b = sparkles_core::store::Store::in_memory(StoreOptions::default());
            let base = crate::facts::Base::new(b.snapshot(), &[Id::DEFAULT_GRAPH]).unwrap();
            let mut g = Graph::with_base(std::sync::Arc::new(base));
            engine::run_program(&mut g, &info.program, &t, &limits(), &mut w).unwrap();
            let warned = w
                .as_slice()
                .iter()
                .any(|m| m.contains("of the included rule set <owl-rl>"));
            let novalue = info.program.rules.iter().any(|r| {
                r.body
                    .iter()
                    .any(|l| matches!(l, Literal::Builtin(b) if b.kind == B::NoValue))
            });
            assert_eq!(warned, novalue);
        }
        // `owl-rl` with a negation that does not stratify is refused with its cycle
        Err(e) => assert!(
            format!("{e:#}").contains("cycle") || format!("{e:#}").contains("negat"),
            "{e:#}"
        ),
    }
    let text = "@prefix ex: <http://ex.org/> .
.decl r(x, y)
[e: (?x ex:p ?z) -> r(?x, ?y)]
";
    let rules = parse_rules(text).unwrap();
    let mut w = WorkingWarnings::new(&t.budget());
    let info = frontend::compile(&rules, &t, &mut w, false).unwrap();
    assert_eq!(info.program.rules[0].exists.len(), 1);
    assert!(info.program.lower(&t.budget()).is_err());
}

/// A query session over a rule text: a demand whose cone holds a backward rule is not
/// read from a materialization, while one of forward rules is, which the infos' parsed
/// indices decide, and the compiler's warnings reach the session's report.
#[test]
fn sessions_read_the_rule_text() {
    use crate::query::QueryReasoner;
    use crate::{Extras, Inputs, Profile};
    use sparkles_core::sparql::reasoning::{Demand, DemandTerm, ReasoningSession, SessionRequest};
    let text = "@prefix ex: <http://ex.org/> .
.decl r(x)
r(?x) :- (?x ex:p ?y).
[f: r(?x) -> (?x ex:f ex:yes)]
[b: (?x ex:b ex:yes) <- r(?x)]
[skip: (?x ex:p ?y), noSuchBuiltin(?x) -> (?x ex:g ?y)]
";
    let s = Store::in_memory(StoreOptions::default());
    let qr = QueryReasoner::new(
        Profile::Rules(text.into()),
        &Extras::default(),
        Inputs::default(),
    )
    .unwrap();
    let req = SessionRequest {
        snap: s.snapshot(),
        readable: None,
        stop: std::sync::Arc::new(|| Ok(())),
        max_memory: None,
        no_cache: true,
    };
    let mut session = qr.test_session(req, true).unwrap();
    let iri = |l: &str| {
        oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(format!(
            "http://ex.org/{l}"
        )))
    };
    let d = |p: &str| Demand {
        s: DemandTerm::Var,
        p: DemandTerm::Const(iri(p)),
        o: DemandTerm::Var,
    };
    let infos = session.prepare(&[d("f"), d("b")], false).unwrap();
    assert!(infos[0].materialized, "{:?}", infos[0].rules);
    assert!(!infos[1].materialized, "{:?}", infos[1].rules);
    let report = session.report();
    assert!(
        report.warnings.iter().any(|w| w.contains("noSuchBuiltin")),
        "{:?}",
        report.warnings
    );
}
