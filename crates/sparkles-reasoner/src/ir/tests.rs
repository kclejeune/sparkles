//! Tests of the rule IR's checks, strata and weak acyclicity (item 839's plan, Phase 1).

use super::*;
use crate::builtins::BuiltinKind as B;
use crate::engine::Slot::{Any, Const as C, Var as V};
use sparkles_core::store::{Store, StoreOptions};

const TYPE: u64 = 7;
const FIRST: u64 = 8;
const REST: u64 = 9;
const NIL: u64 = 10;

fn vocab() -> Vocab {
    Vocab {
        rdf_type: Some(TYPE),
        rdf_first: FIRST,
        rdf_rest: REST,
    }
}

fn tri(s: Slot, p: u64, o: Slot) -> IAtom {
    IAtom::triple(s, C(p), o)
}

fn pos(a: IAtom) -> Literal {
    Literal::Pos(a)
}

fn neg(a: IAtom) -> Literal {
    Literal::Neg(a)
}

fn bi(kind: BuiltinKind, args: &[Slot]) -> Literal {
    Literal::Builtin(IBuiltin::new(kind, args))
}

fn head(a: IAtom) -> Head {
    Head::Atom(a)
}

fn rule(name: &str, body: Vec<Literal>, heads: Vec<Head>) -> Rule {
    let mut nvars = 0;
    let mut see = |s: &Slot| {
        if let V(v) = s {
            nvars = nvars.max(v + 1);
        }
    };
    for l in &body {
        match l {
            Literal::Pos(a) | Literal::Neg(a) => a.args.iter().for_each(&mut see),
            Literal::Builtin(b) => b.args.iter().for_each(&mut see),
        }
    }
    for h in &heads {
        match h {
            Head::Atom(a) => a.args.iter().for_each(&mut see),
            Head::Action(_, args) => args.iter().for_each(&mut see),
        }
    }
    Rule {
        name: name.into(),
        nvars,
        body,
        head: heads,
        exists: Vec::new(),
        vars: Vec::new(),
    }
}

fn exists(mut r: Rule, vars: &[usize]) -> Rule {
    r.exists = vars.to_vec();
    r
}

/// Relations named `a`, `b`, … of the given arities.
fn relations(arities: &[u16]) -> (Relations, Vec<RelId>) {
    let mut rels = Relations::default();
    let ids = arities
        .iter()
        .enumerate()
        .map(|(i, &n)| {
            rels.declare(&((b'a' + i as u8) as char).to_string(), n, false)
                .unwrap()
        })
        .collect();
    (rels, ids)
}

fn program(rels: Relations, rules: Vec<Rule>) -> Result<Program, ProgramError> {
    Program::new(rels, rules, &vocab(), &Budget::unlimited())
}

fn invalid(r: Result<Program, ProgramError>) -> IrError {
    match r {
        Err(ProgramError::Invalid(e)) => e,
        Err(ProgramError::Memory(e)) => panic!("memory: {e}"),
        Ok(p) => panic!("accepted: {p:?}"),
    }
}

/// The stratum of the part of rule `name` that holds head `h`.
fn stratum(p: &Program, name: &str, h: usize) -> u32 {
    let ri = p.rules.iter().position(|r| r.name == name).unwrap();
    p.strata
        .parts
        .iter()
        .find(|part| part.rule == ri && part.heads.contains(&h))
        .unwrap()
        .stratum
}

// ------------------------------------------------------------------- strata ---

#[test]
fn negations_across_relations_give_increasing_strata() {
    let (rels, r) = relations(&[1, 1, 1, 1]);
    let x = V(0);
    let p = program(
        rels,
        vec![
            rule(
                "b",
                vec![pos(IAtom::rel(r[0], &[x]))],
                vec![head(IAtom::rel(r[1], &[x]))],
            ),
            rule(
                "c",
                vec![pos(IAtom::rel(r[0], &[x])), neg(IAtom::rel(r[1], &[x]))],
                vec![head(IAtom::rel(r[2], &[x]))],
            ),
            rule(
                "d",
                vec![pos(IAtom::rel(r[0], &[x])), neg(IAtom::rel(r[2], &[x]))],
                vec![head(IAtom::rel(r[3], &[x]))],
            ),
        ],
    )
    .unwrap();
    assert_eq!(p.strata.stratified, Ok(()));
    assert_eq!(
        (
            stratum(&p, "b", 0),
            stratum(&p, "c", 0),
            stratum(&p, "d", 0)
        ),
        (0, 1, 2)
    );
    assert_eq!(p.strata.count, 3);
    assert!(p.strata.has_negation);
}

#[test]
fn negative_cycles_are_refused_with_their_rules() {
    let (rels, r) = relations(&[1, 1, 1]);
    let x = V(0);
    // a negation of the rule's own head
    let e = invalid(program(
        rels.clone(),
        vec![rule(
            "self",
            vec![pos(IAtom::rel(r[0], &[x])), neg(IAtom::rel(r[1], &[x]))],
            vec![head(IAtom::rel(r[1], &[x]))],
        )],
    ));
    let IrError::NegativeCycle(c) = e else {
        panic!("{e}")
    };
    assert_eq!(c.rule, "self");
    assert_eq!(c.rules, vec!["self".to_string()]);
    // through two relations
    let e = invalid(program(
        rels,
        vec![
            rule(
                "one",
                vec![pos(IAtom::rel(r[0], &[x])), neg(IAtom::rel(r[2], &[x]))],
                vec![head(IAtom::rel(r[1], &[x]))],
            ),
            rule(
                "two",
                vec![pos(IAtom::rel(r[1], &[x]))],
                vec![head(IAtom::rel(r[2], &[x]))],
            ),
        ],
    ));
    let IrError::NegativeCycle(c) = e else {
        panic!("{e}")
    };
    assert_eq!(c.rule, "one");
    assert_eq!(c.rules, vec!["one".to_string(), "two".to_string()]);
    assert!(c.to_string().contains("'one', 'two'"), "{c}");
}

#[test]
fn a_negation_out_of_a_positive_cycle_stratifies() {
    let (rels, r) = relations(&[2, 1, 1]);
    let (x, y, z) = (V(0), V(1), V(2));
    let p = program(
        rels,
        vec![
            rule(
                "step",
                vec![pos(tri(x, 100, y))],
                vec![head(IAtom::rel(r[0], &[x, y]))],
            ),
            rule(
                "path",
                vec![
                    pos(IAtom::rel(r[0], &[x, y])),
                    pos(IAtom::rel(r[0], &[y, z])),
                ],
                vec![head(IAtom::rel(r[0], &[x, z]))],
            ),
            rule(
                "lonely",
                vec![pos(IAtom::rel(r[1], &[x])), neg(IAtom::rel(r[0], &[x, x]))],
                vec![head(IAtom::rel(r[2], &[x]))],
            ),
        ],
    )
    .unwrap();
    assert_eq!(stratum(&p, "path", 0), 0);
    assert_eq!(stratum(&p, "lonely", 0), 1);
}

/// Round 1 of the plan's review: a rule with two heads in different strata is split, so
/// the head that a lower rule reads is derived in the lower stratum.
#[test]
fn rules_without_fresh_terms_split_their_heads() {
    let (rels, r) = relations(&[1, 1, 1, 1, 1]);
    let [a, pp, q, xx, rr] = [r[0], r[1], r[2], r[3], r[4]];
    let x = V(0);
    let p = program(
        rels,
        vec![
            rule(
                "both",
                vec![pos(IAtom::rel(a, &[x]))],
                vec![head(IAtom::rel(pp, &[x])), head(IAtom::rel(q, &[x]))],
            ),
            rule(
                "neg",
                vec![pos(IAtom::rel(a, &[x])), neg(IAtom::rel(xx, &[x]))],
                vec![head(IAtom::rel(pp, &[x]))],
            ),
            rule(
                "r",
                vec![pos(IAtom::rel(q, &[x]))],
                vec![head(IAtom::rel(rr, &[x]))],
            ),
        ],
    )
    .unwrap();
    assert_eq!(stratum(&p, "both", 0), 1);
    assert_eq!(stratum(&p, "both", 1), 0);
    assert_eq!(stratum(&p, "r", 0), 0);
}

/// Round 4 and 5 of the review: heads that share a new node stay in one part, also when
/// the node reaches them through `makeSkolem`, and an existential rule keeps its heads.
#[test]
fn rules_with_fresh_terms_keep_their_heads_together() {
    let (rels, r) = relations(&[1, 2, 2, 1, 1, 2, 2]);
    let [a, pp, q, xx, joined, s2, s3] = [r[0], r[1], r[2], r[3], r[4], r[5], r[6]];
    let (x, y, t) = (V(0), V(1), V(2));
    let others = || {
        vec![
            rule(
                "neg",
                vec![pos(IAtom::rel(a, &[x])), neg(IAtom::rel(xx, &[x]))],
                vec![head(IAtom::rel(pp, &[x, x]))],
            ),
            rule(
                "join",
                vec![pos(IAtom::rel(pp, &[x, y])), pos(IAtom::rel(q, &[x, y]))],
                vec![head(IAtom::rel(joined, &[x]))],
            ),
        ]
    };
    let heads = || vec![head(IAtom::rel(pp, &[x, y])), head(IAtom::rel(q, &[x, y]))];
    let check = |first: Rule, together: bool| {
        let mut rules = vec![first];
        rules.extend(others());
        let p = program(rels.clone(), rules).unwrap();
        let parts = p.strata.parts.iter().filter(|part| part.rule == 0).count();
        assert_eq!(parts, if together { 1 } else { 2 });
        if together {
            assert_eq!(stratum(&p, "first", 0), 1);
            assert_eq!(stratum(&p, "first", 1), 1);
            assert_eq!(stratum(&p, "join", 0), 1);
        } else {
            assert_eq!(stratum(&p, "first", 1), 0);
        }
    };
    check(
        rule(
            "first",
            vec![pos(IAtom::rel(a, &[x])), bi(B::MakeTemp, &[y])],
            heads(),
        ),
        true,
    );
    check(
        rule(
            "first",
            vec![
                pos(IAtom::rel(a, &[x])),
                bi(B::MakeTemp, &[t]),
                bi(B::MakeSkolem, &[y, t]),
            ],
            heads(),
        ),
        true,
    );
    check(
        rule(
            "first",
            vec![pos(IAtom::rel(a, &[x])), bi(B::MakeSkolem, &[y, x])],
            heads(),
        ),
        false,
    );
    // an existential rule is one part
    let p = program(
        rels.clone(),
        vec![exists(
            rule(
                "ex",
                vec![pos(IAtom::rel(a, &[x]))],
                vec![head(IAtom::rel(s2, &[x, y])), head(IAtom::rel(s3, &[x, t]))],
            ),
            &[1, 2],
        )],
    )
    .unwrap();
    assert_eq!(p.strata.parts.len(), 1);
    assert_eq!(p.strata.parts[0].heads.as_slice(), &[0, 1]);
}

#[test]
fn no_value_reads_a_derived_constant_predicate_below() {
    let (x, y) = (V(0), V(1));
    let p = program(
        Relations::default(),
        vec![
            rule("q", vec![pos(tri(x, 100, y))], vec![head(tri(x, 101, y))]),
            rule(
                "n",
                vec![pos(tri(x, 100, y)), bi(B::NoValue, &[x, C(101), Any])],
                vec![head(tri(x, 102, y))],
            ),
        ],
    )
    .unwrap();
    assert_eq!(p.strata.stratified, Ok(()));
    assert_eq!((stratum(&p, "q", 0), stratum(&p, "n", 0)), (0, 1));
}

/// A head with a variable predicate gives every predicate, so a negation of any derived
/// predicate in the same program lies on its cycle.
#[test]
fn a_variable_predicate_head_gives_every_predicate() {
    let (x, y, pv) = (V(0), V(1), V(2));
    let rules = vec![
        rule(
            "sub",
            vec![pos(tri(pv, 200, V(3))), pos(IAtom::triple(x, pv, y))],
            vec![head(IAtom::triple(x, V(3), y))],
        ),
        rule(
            "n",
            vec![pos(tri(x, 100, y)), bi(B::NoValue, &[x, C(101), Any])],
            vec![head(tri(x, 102, y))],
        ),
    ];
    // Jena rules alone keep Jena's reading of noValue: reported, not refused
    let p = program(Relations::default(), rules.clone()).unwrap();
    assert!(p.strata.stratified.is_err());
    // with an IR feature, the whole program must stratify
    let (rels, r) = relations(&[1]);
    let mut with_ir = rules;
    with_ir.push(rule(
        "rel",
        vec![pos(tri(x, 100, y))],
        vec![head(IAtom::rel(r[0], &[x]))],
    ));
    assert!(matches!(
        invalid(program(rels, with_ir)),
        IrError::NegativeCycle(_)
    ));
}

/// Round 1 of the review: an unrelated negated atom puts a program under the strata, and
/// a `noValue` cycle elsewhere in it then refuses it.
#[test]
fn an_unrelated_negation_refuses_a_no_value_cycle() {
    let (x, y) = (V(0), V(1));
    let cycle = vec![
        rule(
            "r",
            vec![pos(tri(x, 100, y)), bi(B::NoValue, &[x, C(101), C(300)])],
            vec![head(tri(x, 101, y))],
        ),
        rule(
            "s",
            vec![pos(tri(x, 101, y))],
            vec![head(tri(x, 101, C(300)))],
        ),
    ];
    let p = program(Relations::default(), cycle.clone()).unwrap();
    let Err(c) = &p.strata.stratified else {
        panic!("a cycle")
    };
    assert_eq!(c.rule, "r");
    let (rels, r) = relations(&[1, 1]);
    let mut more = cycle;
    more.push(rule(
        "other",
        vec![pos(tri(x, 500, y)), neg(IAtom::rel(r[0], &[x]))],
        vec![head(IAtom::rel(r[1], &[x]))],
    ));
    assert!(matches!(
        invalid(program(rels, more)),
        IrError::NegativeCycle(c) if c.rule == "r"
    ));
}

/// Round 4 of the review: `A(x), noValue(x, p) → R(x)` with `R(x) → (x p v)` and `R` a
/// relation is refused, so no admitted program reads its own blocker.
#[test]
fn a_relation_that_gives_its_own_blocker_is_refused() {
    let (rels, r) = relations(&[1]);
    let x = V(0);
    let e = invalid(program(
        rels,
        vec![
            rule(
                "make",
                vec![pos(tri(x, TYPE, C(400))), bi(B::NoValue, &[x, C(101), Any])],
                vec![head(IAtom::rel(r[0], &[x]))],
            ),
            rule(
                "block",
                vec![pos(IAtom::rel(r[0], &[x]))],
                vec![head(tri(x, 101, C(401)))],
            ),
        ],
    ));
    assert!(matches!(e, IrError::NegativeCycle(c) if c.rule == "make"));
}

#[test]
fn list_reads_are_positive_and_open_lists_are_reported() {
    let (l, x, y) = (V(0), V(1), V(2));
    let reader = |kind: BuiltinKind| {
        let args = if kind == B::ListLength {
            vec![l, x]
        } else {
            vec![l, C(300)]
        };
        rule(
            "reader",
            vec![pos(tri(y, 100, l)), bi(kind, &args)],
            vec![head(tri(y, 101, C(301)))],
        )
    };
    let builder = rule(
        "builder",
        vec![pos(tri(y, 102, l))],
        vec![head(tri(l, FIRST, y)), head(tri(l, REST, C(NIL)))],
    );
    for kind in [B::ListNotContains, B::ListLength] {
        let p = program(Relations::default(), vec![reader(kind), builder.clone()]).unwrap();
        assert_eq!(p.strata.stratified, Ok(()));
        assert!(!p.strata.has_negation);
        assert_eq!(p.strata.open_lists, vec![0], "{kind:?}");
        // the reader depends on the builder's lists
        assert_eq!(p.strata.count, 1);
        let p = program(Relations::default(), vec![reader(kind)]).unwrap();
        assert!(p.strata.open_lists.is_empty(), "{kind:?}");
    }
    // a positive list builtin is not open
    let p = program(
        Relations::default(),
        vec![
            rule(
                "member",
                vec![pos(tri(y, 100, l)), bi(B::ListMember, &[l, x])],
                vec![head(tri(y, 101, x))],
            ),
            builder,
        ],
    )
    .unwrap();
    assert!(p.strata.open_lists.is_empty());
}

/// A dependency through list structure: a negation of what a list's reader gives, in a
/// rule that builds the list, is a cycle; and `listForAll`'s tested facts are reads.
#[test]
fn lists_and_list_for_all_are_dependencies() {
    let (l, x, y) = (V(0), V(1), V(2));
    let rules = vec![
        rule(
            "build",
            vec![pos(tri(y, 100, l)), bi(B::NoValue, &[y, C(101), Any])],
            vec![head(tri(l, REST, C(NIL)))],
        ),
        rule(
            "read",
            vec![pos(tri(y, 102, l)), bi(B::ListMember, &[l, x])],
            vec![head(tri(y, 101, x))],
        ),
    ];
    let p = program(Relations::default(), rules).unwrap();
    assert!(matches!(&p.strata.stratified, Err(c) if c.rule == "build"));
    let rules = vec![
        rule(
            "tested",
            vec![pos(tri(y, 103, l)), bi(B::NoValue, &[y, C(104), Any])],
            vec![head(tri(y, 105, C(300)))],
        ),
        rule(
            "all",
            vec![pos(tri(y, 102, l)), bi(B::ListForAll, &[l, y, C(105)])],
            vec![head(tri(y, 104, C(300)))],
        ),
    ];
    let p = program(Relations::default(), rules).unwrap();
    assert!(matches!(&p.strata.stratified, Err(c) if c.rule == "tested"));
}

#[test]
fn head_actions_give_their_predicate_and_read_lists() {
    let (l, x, v) = (V(0), V(1), V(2));
    let rules = vec![
        rule(
            "map",
            vec![pos(tri(x, 100, l))],
            vec![Head::Action(
                HeadAction::ListMapAsSubject,
                SmallVec::from_slice(&[l, C(101), C(300)]),
            )],
        ),
        rule(
            "n",
            vec![pos(tri(x, 100, v)), bi(B::NoValue, &[x, C(101), Any])],
            vec![head(tri(x, FIRST, v))],
        ),
    ];
    let p = program(Relations::default(), rules).unwrap();
    // `n` negates what `map` gives, and `map` reads the lists `n` builds
    assert!(matches!(&p.strata.stratified, Err(c) if c.rule == "n"));
}

#[test]
fn constant_classes_are_apart() {
    let x = V(0);
    let p = program(
        Relations::default(),
        vec![
            rule(
                "up",
                vec![
                    pos(tri(x, TYPE, C(400))),
                    bi(B::NoValue, &[x, C(TYPE), C(401)]),
                ],
                vec![head(tri(x, TYPE, C(402)))],
            ),
            rule(
                "lo",
                vec![pos(tri(x, TYPE, C(403)))],
                vec![head(tri(x, TYPE, C(401)))],
            ),
        ],
    )
    .unwrap();
    assert_eq!(p.strata.stratified, Ok(()));
    assert_eq!((stratum(&p, "lo", 0), stratum(&p, "up", 0)), (0, 1));
}

// ------------------------------------------------------------ the profiles ---

fn terms() -> Terms {
    Terms::new(Store::in_memory(StoreOptions::default()).snapshot())
}

fn compiled(text: &str, terms: &Terms) -> Vec<Rule> {
    let rules = crate::parse_rules(text).unwrap();
    let mut w = crate::budget::WorkingWarnings::new(&terms.budget());
    let c = crate::engine::compile_info(&rules, terms, &mut w, true).unwrap();
    c.rules().iter().map(Rule::from_crule).collect()
}

/// The Jena frontend is exact: every IR rule of a profile has its rule's atoms, builtins
/// and heads, and no relation, negation or existential.
#[test]
fn the_jena_frontend_keeps_every_rule() {
    let t = terms();
    for text in [
        crate::RDFS_RULES,
        crate::RDFS_SIMPLE_RULES,
        crate::OWL_RL_RULES,
    ] {
        let rules = crate::parse_rules(text).unwrap();
        let mut w = crate::budget::WorkingWarnings::new(&t.budget());
        let c = crate::engine::compile(&rules, &t, &mut w).unwrap();
        for r in c.iter() {
            let ir = Rule::from_crule(r);
            assert_eq!(ir.name, r.name);
            assert_eq!(ir.nvars, r.nvars);
            assert!(!ir.uses_ir(), "{}", r.name);
            let atoms: Vec<IAtom> = ir
                .body
                .iter()
                .filter_map(|l| match l {
                    Literal::Pos(a) => Some(a.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(atoms.len(), r.atoms.len());
            for (a, b) in atoms.iter().zip(&r.atoms) {
                assert_eq!(a, &IAtom::triple(b.s, b.p, b.o));
            }
            let builtins: Vec<&IBuiltin> = ir
                .body
                .iter()
                .filter_map(|l| match l {
                    Literal::Builtin(b) => Some(b),
                    _ => None,
                })
                .collect();
            assert_eq!(builtins.len(), r.builtins.len());
            for (a, b) in builtins.iter().zip(&r.builtins) {
                assert_eq!(a.kind, b.kind);
                assert_eq!(a.args.as_slice(), b.args.as_slice());
                assert_eq!(a.inputs.as_slice(), b.inputs.as_slice());
                assert_eq!(a.outputs.as_slice(), b.outputs.as_slice());
            }
            assert_eq!(ir.head.len(), r.head.len());
            for (a, b) in ir.head.iter().zip(&r.head) {
                match (a, b) {
                    (Head::Atom(a), CHead::Triple(s, p, o)) => {
                        assert_eq!(a, &IAtom::triple(*s, *p, *o))
                    }
                    (Head::Action(x, args), CHead::Action(y, brgs)) => {
                        assert_eq!(x, y);
                        assert_eq!(args.as_slice(), brgs.as_slice());
                    }
                    _ => panic!("{}: head kinds differ", r.name),
                }
            }
        }
    }
}

/// The built-in profiles have no negation and form one stratum, and `owl-rl`'s negative
/// list builtins are open, since its heads with a variable predicate may give list cells.
#[test]
fn the_profiles_form_one_stratum() {
    let t = terms();
    let v = Vocab::of(&t);
    for (text, open) in [
        (crate::RDFS_RULES, false),
        (crate::RDFS_SIMPLE_RULES, false),
        (crate::OWL_RL_RULES, true),
    ] {
        let rules = compiled(text, &t);
        let p = Program::new(Relations::default(), rules, &v, &t.budget()).unwrap();
        assert_eq!(p.strata.stratified, Ok(()));
        assert!(!p.strata.has_negation);
        assert_eq!(p.strata.count, 1);
        assert_eq!(!p.strata.open_lists.is_empty(), open);
        if open {
            let names: Vec<&str> = p
                .strata
                .open_lists
                .iter()
                .map(|&i| p.rules[i].name.as_str())
                .collect();
            assert!(names.contains(&"cls-int1"), "{names:?}");
        }
    }
}

/// `ir::Strata` finds a negation on a cycle exactly when demand's detector does, on
/// programs without grouping edges, and finds one through them where demand does not.
#[test]
fn strata_agree_with_the_demand_detector() {
    let t = terms();
    let v = Vocab::of(&t);
    let agrees = |text: &str| -> bool {
        let rules = crate::parse_rules(text).unwrap();
        let mut w = crate::budget::WorkingWarnings::new(&t.budget());
        let c = crate::engine::compile_info(&rules, &t, &mut w, true).unwrap();
        let legacy = crate::demand::negation_on_a_cycle(c.rules(), &t);
        let ir: Vec<Rule> = c.rules().iter().map(Rule::from_crule).collect();
        let p = Program::new(Relations::default(), ir, &v, &t.budget()).unwrap();
        assert_eq!(p.strata.stratified.is_err(), legacy, "{text}");
        legacy
    };
    let pre = "@prefix ex: <http://ex.org/>.\n";
    let fixed = [
        // tests/demand.rs
        "[r: (?x ex:q ?y) <- (?x ex:p ?y), noValue(?x, ex:q, ex:c)]
         [s: (?x ex:q ex:c) <- (?x ex:q ?y)]",
        "[q: (?a ex:p ?b) -> (?a ex:q ?b)]
         [n: (?a ex:p ?b), noValue(?a, ex:q) -> (ex:C rdfs:subClassOf ex:D)]
         [sc: (?c rdfs:subClassOf ?d), (?x rdf:type ?c) -> (?x rdf:type ?d)]",
        "[u: (?x ex:p ?y), noValue(?x, rdf:type, ex:Lower) -> (?x rdf:type ex:Upper)]
         [v: (?x ex:s ?y), noValue(?x, rdf:type, ex:Lower) -> (?x rdf:type ex:Upper)]",
        "[l: (?x ex:p ?y), noValue(ex:a, ex:q, ex:b) -> (ex:L rdf:first ex:b), (ex:L rdf:rest rdf:nil)]
         [m: (?x ex:p ?y), listMember(ex:L, ?m) -> (ex:a ex:q ?m)]",
        "[copy: (?x ex:p ?y), listMember(ex:L0, ?m) -> (ex:L1 rdf:first ?m), (ex:L1 rdf:rest rdf:nil)]
         [use: (?x ex:p ?y), listMember(ex:L0, ?m), noValue(?x, rdf:type, ex:Lower) -> (?x rdf:type ex:Upper)]",
        "[u: (?x ex:p ?y), noValue(ex:a, rdf:type, ex:Lower) -> (ex:a rdf:type ex:Upper)]
         [lo: (?z ex:p ?y), listMember(ex:L1, ?x) -> (?x rdf:type ex:Lower)]
         [mk: (?x ex:requires ?ls), listForAll(?ls, ?x, rdf:type) -> (ex:L1 rdf:first ?x), (ex:L1 rdf:rest rdf:nil)]",
        // round 4 of the plan's review: a head action's list reads belong to every head
        "[both: (?x ex:a ?v) -> (?x ex:p ?v), listMapAsSubject(?x, ex:q, ?v)]
         [n: (?x ex:e ?v), noValue(?x, ex:p) -> (?x rdf:first ?v)]",
    ];
    let cycles: Vec<bool> = fixed
        .iter()
        .map(|f| {
            agrees(&format!(
                "{pre}@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>.\n{f}"
            ))
        })
        .collect();
    assert_eq!(cycles, [true, false, false, true, false, true, true]);
    // round 5: the grouping edges of a `makeTemp` rule are the one difference
    let text = format!(
        "{pre}[t: (?x ex:a ?v), makeTemp(?y) -> (?x ex:p ?y), (?x ex:q ?y)]
         [n: (?x ex:a ?v), noValue(?x, ex:p) -> (?x ex:q ?v)]"
    );
    let rules = crate::parse_rules(&text).unwrap();
    let mut w = crate::budget::WorkingWarnings::new(&t.budget());
    let c = crate::engine::compile_info(&rules, &t, &mut w, true).unwrap();
    assert!(!crate::demand::negation_on_a_cycle(c.rules(), &t));
    let ir: Vec<Rule> = c.rules().iter().map(Rule::from_crule).collect();
    let p = Program::new(Relations::default(), ir, &v, &t.budget()).unwrap();
    assert!(matches!(&p.strata.stratified, Err(c) if c.rule == "n"));
    // random programs over a few predicates and classes
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let mut found = [0usize; 2];
    for _ in 0..500 {
        let mut text = String::from(pre);
        for ri in 0..(2 + next(4)) {
            let pat = |next: &mut dyn FnMut(u64) -> u64| -> String {
                match next(6) {
                    0 => format!("(?x rdf:type ex:C{})", next(3)),
                    1 => "(?x rdf:type ?c)".into(),
                    2 => "(?x ?p ?y)".into(),
                    3 => format!("(?x rdf:first ex:m{})", next(2)),
                    _ => format!("(?x ex:p{} ?y)", next(4)),
                }
            };
            let mut body = vec![
                format!("(?x ex:p{} ?y)", next(4)),
                "(?y ex:pp ?p)".into(),
                "(?x ex:cc ?c)".into(),
            ];
            match next(5) {
                0 => body.push(format!("noValue(?x, ex:p{})", next(4))),
                1 => body.push(format!("noValue(?x, rdf:type, ex:C{})", next(3))),
                2 => body.push("listMember(?y, ?m)".into()),
                3 => body.push(format!("listForAll(?y, ?x, ex:p{})", next(4))),
                _ => {}
            }
            if next(3) == 0 {
                body.push(format!("noValue(?y, ex:p{})", next(4)));
            }
            let heads = (0..1 + next(2)).map(|_| pat(&mut next)).collect::<Vec<_>>();
            text.push_str(&format!(
                "[r{ri}: {} -> {}]\n",
                body.join(", "),
                heads.join(", ")
            ));
        }
        found[agrees(&text) as usize] += 1;
    }
    // both answers occur, so the comparison means something
    assert!(found[0] > 50 && found[1] > 50, "{found:?}");
}

// ------------------------------------------------------- weak acyclicity ---

fn acyclic(rels: Relations, rules: Vec<Rule>) -> Result<(), String> {
    match program(rels, rules) {
        Ok(_) => Ok(()),
        Err(ProgramError::Invalid(IrError::NotWeaklyAcyclic(c))) => Err(c.to_string()),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn fagins_example_is_refused() {
    let (rels, r) = relations(&[2]);
    let (x, y, z) = (V(0), V(1), V(2));
    let e = acyclic(
        rels,
        vec![exists(
            rule(
                "succ",
                vec![pos(IAtom::rel(r[0], &[x, y]))],
                vec![head(IAtom::rel(r[0], &[y, z]))],
            ),
            &[2],
        )],
    )
    .unwrap_err();
    assert!(
        e.contains("rule 'succ'") && e.contains("column 2 of relation 'a'"),
        "{e}"
    );
}

#[test]
fn triple_positions_are_kept_apart_by_predicate() {
    let (x, y, z) = (V(0), V(1), V(2));
    let r = |p: u64, q: u64| {
        exists(
            rule("r", vec![pos(tri(x, p, y))], vec![head(tri(y, q, z))]),
            &[2],
        )
    };
    acyclic(Relations::default(), vec![r(100, 101)]).unwrap();
    acyclic(Relations::default(), vec![r(100, 100)]).unwrap_err();
    // a head with a variable predicate gives every predicate: through it the new value
    // reaches the existential rule again
    let back = rule(
        "back",
        vec![pos(tri(x, 101, y)), pos(tri(x, 200, V(3)))],
        vec![head(IAtom::triple(x, V(3), y))],
    );
    acyclic(Relations::default(), vec![r(100, 101), back]).unwrap_err();
    // and without the feedback to the rule's own predicate, it does not
    let elsewhere = rule(
        "elsewhere",
        vec![pos(tri(x, 102, y)), pos(tri(x, 200, V(3)))],
        vec![head(IAtom::triple(x, V(3), y))],
    );
    acyclic(Relations::default(), vec![r(100, 101), elsewhere]).unwrap();
    // a variable-predicate atom reads every predicate, and a constant head of another
    // predicate is apart from it only if nothing reads it back
    let read_all = rule(
        "read",
        vec![pos(IAtom::triple(x, V(3), y))],
        vec![head(tri(x, 300, y))],
    );
    acyclic(Relations::default(), vec![r(100, 101), read_all]).unwrap();
}

#[test]
fn chains_and_normal_cycles_are_accepted() {
    let (rels, r) = relations(&[1, 2, 2]);
    let (x, y, z) = (V(0), V(1), V(2));
    acyclic(
        rels.clone(),
        vec![
            exists(
                rule(
                    "one",
                    vec![pos(IAtom::rel(r[0], &[x]))],
                    vec![head(IAtom::rel(r[1], &[x, y]))],
                ),
                &[1],
            ),
            exists(
                rule(
                    "two",
                    vec![pos(IAtom::rel(r[1], &[x, y]))],
                    vec![head(IAtom::rel(r[2], &[y, z]))],
                ),
                &[2],
            ),
            // a normal cycle, without a special edge
            rule(
                "sym",
                vec![pos(IAtom::rel(r[2], &[x, y]))],
                vec![head(IAtom::rel(r[2], &[y, x]))],
            ),
        ],
    )
    .unwrap();
}

#[test]
fn an_existential_predicate_is_refused() {
    let (x, y) = (V(0), V(1));
    let e = invalid(program(
        Relations::default(),
        vec![exists(
            rule(
                "p",
                vec![pos(tri(x, 100, x))],
                vec![head(IAtom::triple(x, y, x))],
            ),
            &[1],
        )],
    ));
    assert!(matches!(e, IrError::ExistentialPredicate { .. }), "{e}");
}

/// Rounds 1 and 2 of the review: builtins that make values, and list members, take part.
#[test]
fn builtins_take_part_in_weak_acyclicity() {
    let (rels, r) = relations(&[1, 2]);
    let (x, y, z) = (V(0), V(1), V(2));
    let ex = exists(
        rule(
            "ex",
            vec![pos(IAtom::rel(r[0], &[x]))],
            vec![head(IAtom::rel(r[1], &[x, z]))],
        ),
        &[2],
    );
    // R(x), makeTemp(y) → R(y), with an existential rule in the program
    let temp = rule(
        "temp",
        vec![pos(IAtom::rel(r[0], &[x])), bi(B::MakeTemp, &[y])],
        vec![head(IAtom::rel(r[0], &[y]))],
    );
    acyclic(rels.clone(), vec![ex.clone(), temp]).unwrap_err();
    // R(x), sum(x, 1, y) → R(y)
    let sum = rule(
        "sum",
        vec![pos(IAtom::rel(r[0], &[x])), bi(B::Sum, &[x, C(500), y])],
        vec![head(IAtom::rel(r[0], &[y]))],
    );
    acyclic(rels.clone(), vec![ex.clone(), sum]).unwrap_err();
    // R(x), listMember(x, y) → R(y), ∃z (y rdf:first z), (y rdf:rest rdf:nil)
    let lists = exists(
        rule(
            "lists",
            vec![pos(IAtom::rel(r[0], &[x])), bi(B::ListMember, &[x, y])],
            vec![
                head(IAtom::rel(r[0], &[y])),
                head(tri(y, FIRST, z)),
                head(tri(y, REST, C(NIL))),
            ],
        ),
        &[2],
    );
    acyclic(rels.clone(), vec![lists]).unwrap_err();
    // listMember over lists no existential makes
    let member = rule(
        "member",
        vec![pos(IAtom::rel(r[0], &[x])), bi(B::ListMember, &[x, y])],
        vec![head(IAtom::rel(r[0], &[y]))],
    );
    acyclic(rels, vec![ex, member]).unwrap();
}

/// Round 3 of the review: a head action gives facts whose subject is a list member.
#[test]
fn head_actions_take_part_in_weak_acyclicity() {
    let (rels, r) = relations(&[1]);
    let (x, y, z) = (V(0), V(1), V(2));
    let make = exists(
        rule(
            "make",
            vec![pos(IAtom::rel(r[0], &[x]))],
            vec![head(tri(x, FIRST, z)), head(tri(x, REST, C(NIL)))],
        ),
        &[2],
    );
    let map = rule(
        "map",
        vec![pos(IAtom::rel(r[0], &[x]))],
        vec![Head::Action(
            HeadAction::ListMapAsSubject,
            SmallVec::from_slice(&[x, C(101), C(300)]),
        )],
    );
    let back = rule(
        "back",
        vec![pos(tri(y, 101, C(300)))],
        vec![head(IAtom::rel(r[0], &[y]))],
    );
    acyclic(rels.clone(), vec![make.clone(), map.clone(), back]).unwrap_err();
    acyclic(rels, vec![make, map]).unwrap();
}

// ------------------------------------------------- relations and safety ---

#[test]
fn declarations_are_checked() {
    let mut rels = Relations::default();
    assert!(rels.is_empty());
    let a = rels.declare("a", 2, false).unwrap();
    assert_eq!(rels.find("a"), Some(a));
    assert_eq!(rels.find("b"), None);
    assert_eq!(rels.declare("a", 2, false).unwrap(), a);
    assert_eq!(
        rels.declare("a", 3, false).unwrap_err(),
        IrError::Redeclared {
            name: "a".into(),
            arity: 2,
            again: 3
        }
    );
    assert!(matches!(
        rels.declare("urn:x-sparkles:rel:a", 1, false),
        Err(IrError::ReservedName(_))
    ));
    let unit = rels.declare("unit", 0, true).unwrap();
    assert_eq!(rels.get(unit).unwrap().arity, 0);
    let x = V(0);
    // a propositional relation in a rule
    let p = program(
        rels.clone(),
        vec![rule(
            "prop",
            vec![pos(IAtom::rel(unit, &[])), pos(tri(x, 100, x))],
            vec![head(IAtom::rel(a, &[x, x]))],
        )],
    )
    .unwrap();
    assert!(p.uses_ir());
    let e = invalid(program(
        rels,
        vec![rule(
            "bad",
            vec![pos(IAtom::rel(a, &[x]))],
            vec![head(tri(x, 100, x))],
        )],
    ));
    assert!(
        matches!(
            e,
            IrError::Arity {
                arity: 2,
                got: 1,
                ..
            }
        ),
        "{e}"
    );
}

#[test]
fn unsafe_and_unschedulable_rules_are_refused() {
    let (rels, r) = relations(&[1, 1]);
    let (x, y, z) = (V(0), V(1), V(2));
    let mut named = rule(
        "head",
        vec![pos(IAtom::rel(r[0], &[x]))],
        vec![head(IAtom::rel(r[1], &[y]))],
    );
    named.vars = vec!["x".into(), "y".into()];
    let e = invalid(program(rels.clone(), vec![named]));
    assert!(e.to_string().contains("?y of the head"), "{e}");
    let e = invalid(program(
        rels.clone(),
        vec![rule(
            "neg",
            vec![pos(IAtom::rel(r[0], &[x])), neg(IAtom::rel(r[1], &[y]))],
            vec![head(IAtom::rel(r[1], &[x]))],
        )],
    ));
    assert!(
        matches!(
            e,
            IrError::Unsafe {
                place: "a negated atom",
                ..
            }
        ),
        "{e}"
    );
    let e = invalid(program(
        rels.clone(),
        vec![rule(
            "input",
            vec![pos(IAtom::rel(r[0], &[x])), bi(B::NotEqual, &[x, y])],
            vec![head(IAtom::rel(r[1], &[x]))],
        )],
    ));
    assert!(matches!(e, IrError::Unschedulable { .. }), "{e}");
    // two builtins whose outputs feed each other
    let e = invalid(program(
        rels.clone(),
        vec![rule(
            "loop",
            vec![
                pos(IAtom::rel(r[0], &[x])),
                bi(B::Sum, &[y, C(1), z]),
                bi(B::Sum, &[z, C(1), y]),
            ],
            vec![head(IAtom::rel(r[1], &[y]))],
        )],
    ));
    assert!(matches!(e, IrError::Unschedulable { .. }), "{e}");
    let e = invalid(program(
        rels,
        vec![exists(
            rule(
                "bound",
                vec![pos(IAtom::rel(r[0], &[x]))],
                vec![head(IAtom::rel(r[1], &[x]))],
            ),
            &[0],
        )],
    ));
    assert!(matches!(e, IrError::BoundExistential { .. }), "{e}");
}

// ----------------------------------------------------------------- budget ---

#[test]
fn the_analyses_are_charged() {
    let (x, y) = (V(0), V(1));
    let rules = || -> Vec<Rule> {
        (0..200u64)
            .map(|i| {
                rule(
                    "r",
                    vec![pos(tri(x, 50, y)), bi(B::NoValue, &[x, C(100 + i), Any])],
                    vec![head(tri(x, 101 + i, y))],
                )
            })
            .collect()
    };
    let budget = Budget::new(Some(4096));
    let r = Program::new(Relations::default(), rules(), &vocab(), &budget);
    assert!(matches!(r, Err(ProgramError::Memory(_))));
    assert_eq!(budget.used(), 0);
    // the analysis itself, with the rules outside the budget
    let budget = Budget::new(Some(4096));
    assert!(strata::Strata::of(&rules(), &vocab(), &budget).is_err());
    assert_eq!(budget.used(), 0);
    let budget = Budget::unlimited();
    let p = Program::new(Relations::default(), rules(), &vocab(), &budget).unwrap();
    assert_eq!(p.strata.count, 201);
    assert!(budget.used() > 0);
    let peak = budget.peak();
    drop(p);
    assert_eq!(budget.used(), 0);
    assert!(peak > 0);
}

/// A program of 50,000 rules in one component, as a demand program can have, stratifies
/// without a deep recursion, under a bound of its size.
#[test]
fn a_large_component_stratifies() {
    let (x, y) = (V(0), V(1));
    let n = 50_000u64;
    let rules: Vec<Rule> = (0..n)
        .map(|i| {
            rule(
                "r",
                vec![pos(tri(x, 1000 + i, y))],
                vec![head(tri(x, 1000 + (i + 1) % n, y))],
            )
        })
        .collect();
    let budget = Budget::unlimited();
    let p = Program::new(Relations::default(), rules, &vocab(), &budget).unwrap();
    assert_eq!(p.strata.count, 1);
    assert_eq!(p.strata.stratified, Ok(()));
    assert!(budget.peak() < 96 << 20, "{}", budget.peak());
}
