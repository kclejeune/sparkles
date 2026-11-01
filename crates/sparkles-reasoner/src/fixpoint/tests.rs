//! The rule program on F13's fixed-point operator against a closure computed from
//! scratch, through random histories with cycles of classes and symmetric relations.

use super::*;
use crate::parser::parse_rules;
use sparkles_core::incr::DeltaSet;
use sparkles_core::incr::fixpoint::FixedPoint;
use sparkles_core::sparql::{QueryOptions, update::update};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::BTreeSet;

const RULES: &str = r#"
[sc: (?x rdf:type ?c) (?c rdfs:subClassOf ?d) -> (?x rdf:type ?d)]
[tr: (?c rdfs:subClassOf ?d) (?d rdfs:subClassOf ?e) -> (?c rdfs:subClassOf ?e)]
[sym: (?a <http://ex/knows> ?b) -> (?b <http://ex/knows> ?a)]
[two: (?a <http://ex/knows> ?b) (?b rdf:type <http://ex/c0>) -> (?a <http://ex/near> ?b) (?b <http://ex/near> ?a)]
"#;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";

/// A triple of the model: kind (0 type, 1 subclass, 2 knows) and two numbers.
type T = (u8, u8, u8);

fn text(t: &T) -> String {
    match t.0 {
        0 => format!("<http://ex/i{}> <{RDF_TYPE}> <http://ex/c{}> .", t.1, t.2),
        1 => format!("<http://ex/c{}> <{SUBCLASS}> <http://ex/c{}> .", t.1, t.2),
        _ => format!(
            "<http://ex/i{}> <http://ex/knows> <http://ex/i{}> .",
            t.1, t.2
        ),
    }
}

/// A small deterministic generator, so the tests need no random crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }

    fn triple(&mut self) -> T {
        (self.next(3) as u8, self.next(4) as u8, self.next(4) as u8)
    }
}

/// The closure of the model's triples under the rules, by naive iteration.
fn closure(base: &BTreeSet<T>) -> BTreeSet<String> {
    let mut facts: BTreeSet<(String, String, String)> = BTreeSet::new();
    for t in base {
        let s = text(t);
        let parts: Vec<&str> = s.trim_end_matches(" .").split(' ').collect();
        facts.insert((parts[0].into(), parts[1].into(), parts[2].into()));
    }
    let ty = format!("<{RDF_TYPE}>");
    let sc = format!("<{SUBCLASS}>");
    let knows = "<http://ex/knows>".to_string();
    let near = "<http://ex/near>".to_string();
    let c0 = "<http://ex/c0>".to_string();
    loop {
        let mut new = Vec::new();
        for (a, p, b) in &facts {
            for (c, q, d) in &facts {
                if *p == ty && *q == sc && b == c {
                    new.push((a.clone(), ty.clone(), d.clone()));
                }
                if *p == sc && *q == sc && b == c {
                    new.push((a.clone(), sc.clone(), d.clone()));
                }
                if *p == knows && *q == ty && b == c && *d == c0 {
                    new.push((a.clone(), near.clone(), b.clone()));
                    new.push((b.clone(), near.clone(), a.clone()));
                }
            }
            if *p == knows {
                new.push((b.clone(), knows.clone(), a.clone()));
            }
        }
        let before = facts.len();
        facts.extend(new);
        if facts.len() == before {
            break;
        }
    }
    facts
        .into_iter()
        .map(|(a, p, b)| format!("{a} {p} {b}"))
        .collect()
}

fn shown(snap: &Snapshot, fp: &FixedPoint<RuleProgram>) -> BTreeSet<String> {
    fp.facts()
        .map(|(t, _)| {
            t.iter()
                .map(|&i| fp.program.term(snap, i).expect("a term").to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

fn apply(s: &Store, del: &[T], ins: &[T]) {
    let mut ops = Vec::new();
    if !del.is_empty() {
        ops.push(format!(
            "DELETE DATA {{ {} }}",
            del.iter().map(text).collect::<String>()
        ));
    }
    if !ins.is_empty() {
        ops.push(format!(
            "INSERT DATA {{ {} }}",
            ins.iter().map(text).collect::<String>()
        ));
    }
    if !ops.is_empty() {
        update(s, &ops.join(" ; "), &QueryOptions::default()).expect("update");
    }
}

fn triples(q: &[[Id; 4]]) -> Vec<Triple> {
    q.iter()
        .filter(|q| q[3] == Id::DEFAULT_GRAPH)
        .map(|q| [q[0], q[1], q[2]])
        .collect()
}

/// Advance `fp` from commit `after` to the store's head.
fn advance(
    s: &Store,
    fp: &mut FixedPoint<RuleProgram>,
    after: u64,
) -> Option<Vec<fixpoint::Change<Triple>>> {
    let control = Control::none();
    let d = DeltaSet::read(s, after, &control);
    assert!(d.complete);
    let (added, removed) = (triples(&d.added), triples(&d.removed));
    let mut cx = RuleCx {
        new: d.snapshot.clone(),
        old: d.old().expect("old snapshot"),
        added: &added,
        removed: &removed,
        control: &control,
    };
    if fp.program.begin(&cx) {
        // a head constant entered the store: build again
        fp.build(&mut cx, None).expect("build");
        return None;
    }
    Some(fp.advance(&mut cx, None).expect("advance"))
}

fn build(s: &Store, mode: Provenance) -> FixedPoint<RuleProgram> {
    let rules = parse_rules(RULES).expect("rules");
    let mut fp = FixedPoint::new(RuleProgram::new(&rules).expect("program"));
    fp.set_provenance(mode);
    let control = Control::none();
    let snap = s.snapshot();
    let mut cx = RuleCx {
        new: snap.clone(),
        old: snap,
        added: &[],
        removed: &[],
        control: &control,
    };
    fp.program.begin(&cx);
    fp.build(&mut cx, None).expect("build");
    fp
}

#[test]
fn rules_are_maintained_as_their_closure() {
    for seed in 1..40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let s = Store::in_memory(StoreOptions::default());
        let mut model: BTreeSet<T> = BTreeSet::new();
        let init: Vec<T> = (0..8).map(|_| rng.triple()).collect();
        apply(&s, &[], &init);
        model.extend(init);
        let mut fp = build(&s, Provenance::None);
        assert_eq!(
            shown(&s.snapshot(), &fp),
            closure(&model),
            "seed {seed}: build"
        );
        for step in 0..15 {
            let after = s.snapshot().commit;
            let mut del = Vec::new();
            let mut ins = Vec::new();
            for _ in 0..1 + rng.next(3) {
                if !model.is_empty() && rng.next(2) == 0 {
                    let held: Vec<&T> = model.iter().collect();
                    del.push(*held[rng.next(held.len() as u64) as usize]);
                } else {
                    ins.push(rng.triple());
                }
            }
            apply(&s, &del, &ins);
            for d in &del {
                model.remove(d);
            }
            model.extend(ins.iter().copied());
            let before = shown(&s.snapshot(), &fp);
            let changes = advance(&s, &mut fp, after);
            let want = closure(&model);
            assert_eq!(shown(&s.snapshot(), &fp), want, "seed {seed} step {step}");
            // the change lists what came and went, and nothing that stayed
            let Some(changes) = changes else {
                continue;
            };
            let gone = before.difference(&want).count();
            let came = want.difference(&before).count();
            let removed = changes.iter().filter(|c| c.weight < 0).count();
            let added = changes.iter().filter(|c| c.weight > 0).count();
            assert_eq!((removed, added), (gone, came), "seed {seed} step {step}");
        }
    }
}

/// Each triple's witness is real: the explicit triples it names derive it alone.
#[test]
fn rule_witnesses_derive_their_triples() {
    for seed in 1..12u64 {
        let mut rng = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d));
        let s = Store::in_memory(StoreOptions::default());
        let mut model: BTreeSet<T> = BTreeSet::new();
        let init: Vec<T> = (0..8).map(|_| rng.triple()).collect();
        apply(&s, &[], &init);
        model.extend(init);
        let mut fp = build(&s, Provenance::why());
        for step in 0..10 {
            let after = s.snapshot().commit;
            let del: Vec<T> = model.iter().take(rng.next(2) as usize).copied().collect();
            let ins = vec![rng.triple()];
            apply(&s, &del, &ins);
            for d in &del {
                model.remove(d);
            }
            model.extend(ins.iter().copied());
            advance(&s, &mut fp, after);
            let snap = s.snapshot();
            let keys: std::collections::BTreeMap<FactKey, T> = model
                .iter()
                .map(|t| {
                    let parts: Vec<String> = text(t)
                        .trim_end_matches(" .")
                        .split(' ')
                        .map(|x| x.trim_matches(['<', '>']).to_string())
                        .collect();
                    let n = |x: &str| Term::NamedNode(oxrdf::NamedNode::new_unchecked(x));
                    (
                        FactKey::of_terms(&n(&parts[0]), &n(&parts[1]), &n(&parts[2]), None),
                        *t,
                    )
                })
                .collect();
            for (f, _) in fp.facts() {
                let a = fp.annotation(f).expect("an annotation");
                let Ann::Why(w) = a else {
                    panic!("not why");
                };
                assert_eq!(w.supports.len(), 1, "seed {seed} step {step}");
                let witness: BTreeSet<T> = w.supports[0]
                    .0
                    .atoms()
                    .iter()
                    .map(|a| match a {
                        Atom::Fact(k) => *keys.get(k).expect("a witness names a held triple"),
                        Atom::Absent(_) => panic!("a condition"),
                    })
                    .collect();
                let fact = f
                    .iter()
                    .map(|&i| fp.program.term(&snap, i).unwrap().to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(
                    closure(&witness).contains(&fact),
                    "seed {seed} step {step}: {fact} from {witness:?}"
                );
            }
        }
    }
}

#[test]
fn unsupported_rules_are_refused() {
    for r in [
        "[b: (?a <http://ex/p> ?b) notEqual(?a, ?b) -> (?a <http://ex/q> ?b)]",
        "[h: (?a <http://ex/p> ?b) -> (?a <http://ex/q> ?c)]",
        "[w: (?a <http://ex/p> ?b) (?b <http://ex/p> ?c) (?c <http://ex/p> ?d) (?d <http://ex/p> ?e) -> (?a <http://ex/q> ?e)]",
    ] {
        let rules = parse_rules(r).expect("parse");
        assert!(RuleProgram::new(&rules).is_err(), "{r}");
    }
}

fn program(
    rules: &str,
    s: &Store,
    mode: Provenance,
    max: Option<u64>,
) -> sparkles_core::Result<FixedPoint<RuleProgram>> {
    let rules = parse_rules(rules).expect("rules");
    let mut fp = FixedPoint::new(RuleProgram::new(&rules).expect("program"));
    fp.set_provenance(mode);
    let control = Control::none();
    let snap = s.snapshot();
    let mut cx = RuleCx {
        new: snap.clone(),
        old: snap,
        added: &[],
        removed: &[],
        control: &control,
    };
    fp.program.begin(&cx);
    fp.build(&mut cx, max)?;
    Ok(fp)
}

fn store(triples: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    update(
        &s,
        &format!("INSERT DATA {{ {triples} }}"),
        &QueryOptions::default(),
    )
    .expect("update");
    s
}

/// A constant that only derived triples hold matches in the bodies of other rules.
#[test]
fn derived_constants_chain_through_rules() {
    let s = store("<http://ex/a> <http://ex/p> <http://ex/b> .");
    let fp = program(
        "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]
         [qr: (?x <http://ex/q> ?y) -> (?x <http://ex/r> ?y)]",
        &s,
        Provenance::None,
        None,
    )
    .expect("build");
    let got = shown(&s.snapshot(), &fp);
    for p in ["p", "q", "r"] {
        let t = format!("<http://ex/a> <http://ex/{p}> <http://ex/b>");
        assert!(got.contains(&t), "{t} missing from {got:?}");
    }
}

/// A body that names the same premise several times is annotated without reporting a
/// cycle, whichever fact is annotated first.
#[test]
fn repeated_premises_are_not_a_cycle() {
    let body = "(?x <http://ex/q> ?y) ".repeat(12);
    let rules = format!(
        "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]
         [w: {body} -> (?x <http://ex/r> ?y)]"
    );
    for k in 0..8 {
        let filler: String = (0..k)
            .map(|j| format!("<http://ex/f{j}> <http://ex/z> <http://ex/g{j}> . "))
            .collect();
        let s = store(&format!(
            "<http://ex/a> <http://ex/p> <http://ex/b> . {filler}"
        ));
        let fp = program(&rules, &s, Provenance::why(), None)
            .unwrap_or_else(|e| panic!("{k} fillers: {e}"));
        assert_eq!(fp.facts().count(), 3 + k);
    }
}

/// Annotations count against the memory budget of a build.
#[test]
fn annotations_count_against_the_budget() {
    let triples: String = (0..5000)
        .map(|i| format!("<http://ex/i{i}> <http://ex/p> <http://ex/j{i}> . "))
        .collect();
    let s = store(&triples);
    let rules = "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]";
    let plain = program(rules, &s, Provenance::None, None).expect("build");
    let why = program(rules, &s, Provenance::why(), None).expect("build");
    assert!(
        why.bytes() > plain.bytes() + 100_000,
        "{} {}",
        why.bytes(),
        plain.bytes()
    );
    let limit = (plain.bytes() + why.bytes()) / 2;
    assert!(program(rules, &s, Provenance::None, Some(limit)).is_ok());
    let Err(e) = program(rules, &s, Provenance::why(), Some(limit)) else {
        panic!("the annotated build fits the budget");
    };
    assert!(
        matches!(e, sparkles_core::Error::BudgetExceeded(_)),
        "{e:?}"
    );
}

/// A fixed point whose rules derive a constant the store lacks survives a checkpoint.
#[test]
fn local_head_constants_survive_a_checkpoint() {
    let s = store("<http://ex/a> <http://ex/p> <http://ex/b> .");
    let rules = "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> <http://ex/new>)]";
    let fp = program(rules, &s, Provenance::why(), None).expect("build");
    let snap = s.snapshot();
    let bytes = RuleProgram::encode(&fp, &snap).expect("encode");
    let parsed = parse_rules(rules).expect("rules");
    let mut back = FixedPoint::new(RuleProgram::new(&parsed).expect("program"));
    back.set_provenance(Provenance::why());
    RuleProgram::decode(&mut back, &bytes, &snap).expect("decode");
    assert_eq!(shown(&snap, &back), shown(&snap, &fp));
    // the restored program advances like the original
    let after = s.snapshot().commit;
    update(
        &s,
        "INSERT DATA { <http://ex/c> <http://ex/p> <http://ex/d> }",
        &QueryOptions::default(),
    )
    .expect("update");
    assert!(
        advance(&s, &mut back, after).is_some(),
        "advanced, not built again"
    );
    let want: BTreeSet<String> = shown(&snap, &fp)
        .into_iter()
        .chain([
            "<http://ex/c> <http://ex/p> <http://ex/d>".to_string(),
            "<http://ex/c> <http://ex/q> <http://ex/new>".to_string(),
        ])
        .collect();
    assert_eq!(shown(&s.snapshot(), &back), want);
}

/// A restored program notices a local constant that the store came to hold after its
/// checkpoint, and is built again instead of advancing over facts with the old id.
#[test]
fn a_restored_program_sees_its_constants_move() {
    let s = store("<http://ex/a> <http://ex/p> <http://ex/b> .");
    let rules = "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]
                 [qr: (?x <http://ex/q> ?y) -> (?x <http://ex/r> ?y)]";
    let fp = program(rules, &s, Provenance::None, None).expect("build");
    let snap = s.snapshot();
    let bytes = RuleProgram::encode(&fp, &snap).expect("encode");
    let parsed = parse_rules(rules).expect("rules");
    let mut back = FixedPoint::new(RuleProgram::new(&parsed).expect("program"));
    RuleProgram::decode(&mut back, &bytes, &snap).expect("decode");
    let after = snap.commit;
    update(
        &s,
        "DELETE DATA { <http://ex/a> <http://ex/p> <http://ex/b> } ;
         INSERT DATA { <http://ex/c> <http://ex/q> <http://ex/d> }",
        &QueryOptions::default(),
    )
    .expect("update");
    assert!(
        advance(&s, &mut back, after).is_none(),
        "the moved constant builds the program again"
    );
    let want: BTreeSet<String> = [
        "<http://ex/c> <http://ex/q> <http://ex/d>",
        "<http://ex/c> <http://ex/r> <http://ex/d>",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(shown(&s.snapshot(), &back), want);
}
