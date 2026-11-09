//! Behavioral preimages against the tested7b9a4e25 compiler. Inputs are immutable
//! fixture ownership; there are no constants whose Terms charges could hide cost.
use super::*;
use crate::parser::{Clause, Direction, Node, Rule, TriplePattern};
use sparkles_core::store::{Store, StoreOptions};

fn triple(reverse: bool) -> Clause {
    let node = |s: &str| Node::Var(s.into());
    Clause::Triple(TriplePattern {
        subject: node(if reverse { "o" } else { "s" }),
        predicate: node("p"),
        object: node(if reverse { "s" } else { "o" }),
    })
}
fn rules() -> Vec<Rule> {
    (0..64)
        .map(|i| Rule {
            name: Some(format!("{}-{i}", "compiler-owned-name-".repeat(32))),
            body: vec![triple(false)],
            head: vec![triple(true)],
            direction: Direction::Forward,
            line: i + 1,
        })
        .collect()
}
fn nested() -> Vec<Rule> {
    let mut rule = Rule {
        name: None,
        body: vec![triple(false)],
        head: vec![triple(true)],
        direction: Direction::Forward,
        line: 1,
    };
    for depth in 0..8 {
        rule = Rule {
            name: (depth == 7).then(|| "compiler-outer-name-".repeat(32)),
            body: vec![triple(false)],
            head: vec![Clause::Rule(Box::new(rule))],
            direction: Direction::Forward,
            line: depth + 2,
        };
    }
    vec![rule]
}
fn terms() -> Terms {
    Terms::new(Store::in_memory(StoreOptions::default()).snapshot())
}
#[test]
fn compiled_rule_payload_has_an_actual_owner() {
    let terms = terms();
    let budget = terms.budget();
    let input = rules();
    let before = budget.used();
    let compiled = compile(&input, &terms, &mut WorkingWarnings::new(&budget)).unwrap();
    assert_eq!(compiled.len(), 64);
    assert!(
        budget.used() > before,
        "compiled rules returned without owned credit"
    );
    drop(compiled);
    assert_eq!(budget.used(), before);
}
#[test]
fn compiled_info_payload_has_an_actual_owner() {
    let terms = terms();
    let budget = terms.budget();
    let input = rules();
    let before = budget.used();
    let compiled = compile_info(&input, &terms, &mut WorkingWarnings::new(&budget), true).unwrap();
    assert_eq!((compiled.rules().len(), compiled.infos().len()), (64, 64));
    assert!(
        budget.used() > before,
        "compiled infos returned without owned credit"
    );
    drop(compiled);
    assert_eq!(budget.used(), before);
}
#[test]
fn compile_refusal_precedes_flattening() {
    let terms = terms();
    let budget = Budget::new(Some(terms.heap_bytes() + 1024));
    terms.set_budget(&budget).unwrap();
    let input = nested();
    COMPILE_FLATTENS.with(|n| n.set(0));
    let compiled = compile(&input, &terms, &mut WorkingWarnings::new(&budget));
    assert_eq!(
        COMPILE_FLATTENS.with(|n| n.get()),
        0,
        "compile reached flatten before budget admission"
    );
    assert!(
        matches!(compiled, Err(BudgetFailure::Local(_))),
        "original local refusal"
    );
}
#[test]
fn compile_info_refusal_precedes_flattening() {
    let terms = terms();
    let budget = Budget::new(Some(terms.heap_bytes() + 1024));
    terms.set_budget(&budget).unwrap();
    let input = nested();
    COMPILE_FLATTENS.with(|n| n.set(0));
    let compiled = compile_info(&input, &terms, &mut WorkingWarnings::new(&budget), true);
    assert_eq!(
        COMPILE_FLATTENS.with(|n| n.get()),
        0,
        "compile_info reached flatten before budget admission"
    );
    assert!(
        matches!(compiled, Err(BudgetFailure::Local(_))),
        "original local refusal"
    );
}

#[test]
fn retained_regex_keeps_its_actual_credit_after_terms_drop() {
    let pool = sparkles_core::sparql::pool::QueryPool::new(32 << 20);
    let budget = Budget::with_parent(None, Some(pool.work_parent()));
    let store = Store::in_memory(StoreOptions::default());
    let terms = Terms::new_in(store.snapshot(), &budget).unwrap();
    let regex = terms.try_regex("x+").unwrap().unwrap();
    assert!(regex.is_match("xxx"));
    drop(terms);
    assert!(regex.is_match("xxx"), "actual regex payload still lives");
    assert!(
        budget.used() > 0,
        "regex retained after Terms without owned credit"
    );
    drop(regex);
    assert_eq!(budget.used(), 0);
    drop(budget);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn borrowed_nested_flatten_preserves_postorder_names_lines_and_variables() {
    let terms = terms();
    let budget = terms.budget();
    let mut input = nested();
    input[0].head.push(triple(true));
    let mut warnings = WorkingWarnings::new(&budget);
    let compiled = compile_info(&input, &terms, &mut warnings, true).unwrap();
    assert_eq!(compiled.rules().len(), 2);
    assert_eq!(
        compiled.rules()[0].name,
        format!("{}{}", "compiler-outer-name-".repeat(32), "/1".repeat(8))
    );
    assert_eq!(compiled.rules()[0].atoms.len(), 9);
    assert_eq!(compiled.rules()[1].name, "compiler-outer-name-".repeat(32));
    assert_eq!(compiled.rules()[1].atoms.len(), 1);
    assert_eq!(compiled.infos()[0].vars, ["s", "p", "o"]);
    assert_eq!(
        (compiled.infos()[0].line, compiled.infos()[0].parsed),
        (1, 0)
    );
    assert_eq!(
        (compiled.infos()[1].line, compiled.infos()[1].parsed),
        (9, 0)
    );
    assert!(warnings.is_empty());
}

#[test]
fn compiled_parent_refusal_is_not_a_skipped_rule_warning() {
    let terms = terms();
    let pool = sparkles_core::sparql::pool::QueryPool::new(1 << 20);
    let budget = Budget::with_parent(None, Some(pool.work_parent()));
    terms.set_budget(&budget).unwrap();
    let before = budget.used();
    let mut warnings = WorkingWarnings::new(&budget);
    let mut input = rules();
    input[0].name = Some("large-compiler-name".repeat(100_000));
    let result = compile(&input, &terms, &mut warnings);
    assert!(matches!(
        result,
        Err(BudgetFailure::Parent {
            refusal: sparkles_budget::ParentRefused::Limit {
                scope: sparkles_budget::Scope::Work,
                ..
            },
            ..
        })
    ));
    assert!(warnings.is_empty());
    assert_eq!(budget.used(), before);
    // Even an empty private owner retains its Budget Arc and allowed backing
    // slack. Return it before asserting complete final-parent cleanup.
    drop(warnings);
    drop(terms);
    assert_eq!(budget.used(), 0);
    drop(budget);
    assert_eq!(pool.granted(), 0);
}

/// A rule that compiles is not charged for error messages it never makes: a long
/// variable name is charged where the compiled rule keeps it, and not again for the
/// unbound-head message that only a failing rule formats.
#[test]
fn compilation_charges_no_unmade_error_messages() {
    const LEN: usize = 1 << 16;
    let name = "v".repeat(LEN);
    let var = |s: &str| Node::Var(s.into());
    let rule = Rule {
        name: Some("long-variable".into()),
        body: vec![Clause::Triple(TriplePattern {
            subject: var(&name),
            predicate: var("p"),
            object: var("o"),
        })],
        head: vec![Clause::Triple(TriplePattern {
            subject: var("o"),
            predicate: var("p"),
            object: var(&name),
        })],
        direction: Direction::Forward,
        line: 1,
    };
    let terms = terms();
    let budget = terms.budget();
    let before = budget.used();
    let compiled = compile(&[rule], &terms, &mut WorkingWarnings::new(&budget)).unwrap();
    assert_eq!(compiled.len(), 1);
    let peak = budget.peak() - before;
    assert!(
        peak < 3 * LEN,
        "compilation charged {peak} for two {LEN}-byte names"
    );
    drop(compiled);
    assert_eq!(budget.used(), before);
}

/// A rule that fails charges its message while it is formatted and releases it after
/// the warning is recorded.
#[test]
fn a_skipped_rule_charges_its_message() {
    const LEN: usize = 1 << 16;
    let rule = Rule {
        name: Some("unknown-builtin".into()),
        body: vec![
            triple(false),
            Clause::Builtin(crate::parser::BuiltinCall {
                name: "u".repeat(LEN),
                args: vec![Node::Var("s".into())],
            }),
        ],
        head: vec![triple(true)],
        direction: Direction::Forward,
        line: 1,
    };
    let terms = terms();
    let budget = terms.budget();
    let before = budget.used();
    let mut warnings = WorkingWarnings::new(&budget);
    let compiled = compile(&[rule], &terms, &mut warnings).unwrap();
    assert!(compiled.is_empty());
    // The kept builtin name, the message and the recorded warning were live at once.
    assert!(
        budget.peak() - before >= 3 * LEN,
        "peak {}",
        budget.peak() - before
    );
    drop(compiled);
    drop(warnings);
    assert_eq!(budget.used(), before);
}
