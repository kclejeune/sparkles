//! The rule program on F13's fixed-point operator against a closure computed from
//! scratch, through random histories with cycles of classes and symmetric relations.

use super::*;
use crate::parser::parse_rules;
use sparkles_core::incr::DeltaSet;
use sparkles_core::incr::fixpoint::FixedPoint;
use sparkles_core::sparql::{QueryOptions, update::update};
use sparkles_core::store::{Store, StoreOptions};
use std::collections::{BTreeMap, BTreeSet};

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
    shown_of(snap, &fp.program, fp.facts())
}

fn shown_of(
    snap: &Snapshot,
    program: &RuleProgram,
    facts: Vec<(Triple, Record)>,
) -> BTreeSet<String> {
    facts
        .into_iter()
        .map(|(t, _)| {
            t.iter()
                .map(|&i| program.term(snap, i).expect("a term").to_string())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Consolidated changes with their triples shown.
fn change_map(
    snap: &Snapshot,
    program: &RuleProgram,
    changes: &[(Triple, i64)],
) -> BTreeMap<String, i64> {
    changes
        .iter()
        .map(|(t, w)| {
            let t = t
                .iter()
                .map(|&i| program.term(snap, i).expect("a term").to_string())
                .collect::<Vec<_>>()
                .join(" ");
            (t, *w)
        })
        .collect()
}

/// The change from closure `before` to closure `after`.
fn difference(before: &BTreeSet<String>, after: &BTreeSet<String>) -> BTreeMap<String, i64> {
    before
        .difference(after)
        .map(|t| (t.clone(), -1))
        .chain(after.difference(before).map(|t| (t.clone(), 1)))
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
    for batch in [1usize, 3, 1024] {
        maintained_as_closure(batch);
    }
}

/// The one-step program with `batch` facts per batched call against the naive closure.
fn maintained_as_closure(batch: usize) {
    for seed in 1..40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        let s = Store::in_memory(StoreOptions::default());
        let mut model: BTreeSet<T> = BTreeSet::new();
        let init: Vec<T> = (0..8).map(|_| rng.triple()).collect();
        apply(&s, &[], &init);
        model.extend(init);
        let mut fp = build(&s, Provenance::None);
        fp.program.set_batch_len(batch);
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
            assert!(
                changes.iter().all(|c| c.weight.abs() == 1),
                "seed {seed} step {step}"
            );
            assert_eq!(
                change_map(&s.snapshot(), &fp.program, &fixpoint::consolidate(&changes)),
                difference(&before, &want),
                "seed {seed} step {step}"
            );
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
                let a = fp.annotation(&f).expect("an annotation");
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
        assert_eq!(fp.facts().len(), 3 + k);
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

/// The witness that the fixed-point operator records for a triple and the witnesses the
/// reasoner keeps for it (item 416) name input quads alike, and the operator's witness
/// holds one of the reasoner's when the reasoner's are exact, since those are all the
/// minimal supports.
#[test]
fn rule_witnesses_agree_with_the_reasoners() {
    use crate::{Cache, Extras, Incremental, Profile, ReasonOptions, materialize_incremental};
    let mut compared = 0;
    for seed in 1..12u64 {
        let mut rng = Rng(seed.wrapping_mul(0x5851_f42d_4c95_7f2d));
        let s = Store::in_memory(StoreOptions::default());
        let init: Vec<T> = (0..8).map(|_| rng.triple()).collect();
        apply(&s, &[], &init);
        let fp = build(&s, Provenance::why());
        let cache = Cache::default();
        materialize_incremental(
            &s,
            &Profile::Rules(RULES.to_string()),
            &Extras::default(),
            Incremental {
                since: None,
                cache: Some(&cache),
            },
            &ReasonOptions {
                witnesses: Some(256),
                ..Default::default()
            },
        )
        .expect("materialize");
        let snap = s.snapshot();
        for (f, _) in fp.facts() {
            let term = |i: Id| fp.program.term(&snap, i).expect("a term");
            let (Term::NamedNode(sub), Term::NamedNode(p)) = (term(f[0]), term(f[1])) else {
                panic!("named nodes");
            };
            let t = oxrdf::Triple::new(sub, p, term(f[2]));
            let Some(Ann::Why(w)) = fp.annotation(&f) else {
                panic!("not why");
            };
            let ours = w.supports[0].0.atoms();
            let theirs = cache
                .witnesses(&t)
                .unwrap_or_else(|| panic!("seed {seed}: the reasoner keeps no witnesses for {t}"));
            assert!(!theirs.annotation.witnesses.is_empty(), "seed {seed}: {t}");
            if theirs.annotation.truncated {
                continue;
            }
            compared += 1;
            assert!(
                theirs
                    .annotation
                    .witnesses
                    .iter()
                    .any(|x| x.atoms().iter().all(|a| ours.contains(a))),
                "seed {seed}: {t}: the operator's {ours:?} holds none of {:?}",
                theirs.annotation.witnesses
            );
        }
    }
    assert!(compared > 0);
}

// ---- the exact mode -----------------------------------------------------------------

/// An [`IndexedStore`] that counts what the operator and the sinks reserve through it,
/// and refuses past `cap`.
#[derive(Default)]
struct Counting {
    inner: IndexedStore,
    reserved: std::cell::Cell<u64>,
    peak: std::cell::Cell<u64>,
    cap: Option<u64>,
    /// the membership lookups made
    lookups: std::cell::Cell<u64>,
}

impl FactStore<Triple, Record> for Counting {
    type Error = sparkles_core::Error;

    fn contains(&self, f: &Triple) -> bool {
        self.lookups.set(self.lookups.get() + 1);
        self.inner.contains(f)
    }

    fn record(&self, f: &Triple) -> Option<Record> {
        self.inner.record(f)
    }

    fn len(&self) -> usize {
        self.inner.len()
    }

    fn for_each(&self, visit: &mut dyn FnMut(&Triple, &Record)) {
        self.inner.for_each(visit)
    }

    fn bytes(&self) -> u64 {
        self.inner.bytes()
    }

    fn insert(&mut self, f: Triple, r: Record) -> sparkles_core::Result<bool> {
        self.inner.insert(f, r)
    }

    fn set_record(&mut self, f: Triple, r: Record) -> sparkles_core::Result<()> {
        self.inner.set_record(f, r)
    }

    fn remove(&mut self, f: &Triple) -> sparkles_core::Result<bool> {
        self.inner.remove(f)
    }

    fn clear(&mut self) {
        self.inner.clear()
    }

    fn reserve(&self, bytes: u64) -> sparkles_core::Result<()> {
        let now = self.reserved.get() + bytes;
        if let Some(cap) = self.cap
            && now > cap
        {
            return Err(sparkles_core::Error::BudgetExceeded(
                sparkles_core::error::Budget {
                    kind: sparkles_core::error::BudgetKind::Memory,
                    limit: cap,
                    requested: now,
                },
            ));
        }
        self.reserved.set(now);
        self.peak.set(self.peak.get().max(now));
        Ok(())
    }

    fn release(&self, bytes: u64) {
        self.reserved.set(self.reserved.get() - bytes);
    }
}

/// The rules of a [`RuleProgram`] in the operator's exact mode. Its proof is exact by
/// construction: once per pass it derives, from the explicit facts of the new snapshot,
/// every fact whose derived premises the store holds, and answers membership in that set,
/// so no partial search is taken for a negative answer.
struct Exact {
    inner: RuleProgram,
    /// insert the added facts and their consequences itself rather than leave it to the
    /// operator
    own_saturation: bool,
    batch: usize,
    cap: Option<u64>,
    provable: Option<FxHashSet<Triple>>,
    /// the first frontier sorted up (`true`) or down (`false`), or as the delta lists it
    order: Option<bool>,
    /// what the program's own saturation inserted in the pass
    inserted: Vec<Triple>,
    /// what the store had reserved when the first proof of a pass was asked
    reserved_at_prove: Option<u64>,
}

impl Exact {
    fn new(rules: &str, own_saturation: bool, batch: usize) -> Exact {
        let rules = parse_rules(rules).expect("rules");
        Exact {
            inner: RuleProgram::new(&rules).expect("program"),
            own_saturation,
            batch,
            cap: None,
            provable: None,
            order: None,
            inserted: Vec::new(),
            reserved_at_prove: None,
        }
    }

    fn provable(&mut self, cx: &RuleCx, store: &Counting) -> sparkles_core::Result<()> {
        if self.provable.is_some() {
            return Ok(());
        }
        let mut set = IndexedStore::default();
        let mut queue = Vec::new();
        cx.new.for_each_quad_in(&[Id::DEFAULT_GRAPH.0], |q| {
            let t = [q[0], q[1], q[2]];
            if set.insert(t, Record::EXPLICIT)? {
                queue.push(t);
            }
            Ok(())
        })?;
        let mut sink = Sink::unbounded();
        while let Some(f) = queue.pop() {
            self.inner
                .instances_of(&set, &f, &|x| set.contains(x), &mut sink)?;
            let found: Vec<(Triple, Record)> = sink.drain().collect();
            for (g, r) in found {
                if store.contains(&g) && set.insert(g, r)? {
                    queue.push(g);
                }
            }
        }
        self.provable = Some(set.facts.keys().copied().collect());
        Ok(())
    }
}

impl Program for Exact {
    type Fact = Triple;
    type Record = Record;
    type Cx<'c> = RuleCx<'c>;
    type Error = sparkles_core::Error;
    type Store = Counting;

    const EXACT: bool = true;

    fn new_store(&self) -> Counting {
        Counting {
            cap: self.cap,
            ..Default::default()
        }
    }

    fn batch_len(&self) -> usize {
        self.batch
    }

    fn premises(&self, f: &Triple, r: &Record, out: &mut Vec<Triple>) {
        self.inner.premises(f, r, out)
    }

    fn axioms(
        &mut self,
        cx: &mut RuleCx,
        store: &Counting,
        out: &mut Sink<'_, (Triple, Record), sparkles_core::Error>,
    ) -> sparkles_core::Result<()> {
        self.inner.axioms(cx, &store.inner, out)
    }

    fn consequences(
        &mut self,
        _cx: &mut RuleCx,
        store: &Counting,
        _w: Which,
        f: &Triple,
        held: &dyn Fn(&Triple) -> bool,
        out: &mut Sink<'_, (Triple, Record), sparkles_core::Error>,
    ) -> sparkles_core::Result<()> {
        self.inner.instances_of(&store.inner, f, held, out)
    }

    fn prove(
        &mut self,
        cx: &mut RuleCx,
        store: &Counting,
        f: &Triple,
        _held: &dyn Fn(&Triple) -> bool,
    ) -> sparkles_core::Result<Option<Record>> {
        if self.reserved_at_prove.is_none() {
            self.reserved_at_prove = Some(store.reserved.get());
        }
        self.provable(cx, store)?;
        let yes = self.provable.as_ref().expect("computed").contains(f);
        Ok(yes.then_some(Record::EXPLICIT))
    }

    fn broken(
        &mut self,
        _cx: &mut RuleCx,
        _store: &Counting,
        _recorded: &dyn Fn(&Triple) -> Option<Record>,
        _out: &mut Sink<'_, Triple, sparkles_core::Error>,
    ) -> sparkles_core::Result<()> {
        panic!("exact mode asks for no broken facts")
    }

    fn added(
        &mut self,
        cx: &mut RuleCx,
        store: &Counting,
        held: &dyn Fn(&Triple) -> bool,
        out: &mut Sink<'_, (Triple, Record), sparkles_core::Error>,
    ) -> sparkles_core::Result<()> {
        self.inner.added(cx, &store.inner, held, out)
    }

    fn frontier(
        &mut self,
        cx: &mut RuleCx,
        store: &Counting,
        out: &mut Sink<'_, Triple, sparkles_core::Error>,
    ) -> sparkles_core::Result<()> {
        let mut removed: Vec<Triple> = cx
            .removed
            .iter()
            .copied()
            .filter(|t| store.contains(t))
            .collect();
        match self.order {
            Some(true) => removed.sort_unstable(),
            Some(false) => removed.sort_unstable_by(|a, b| b.cmp(a)),
            None => {}
        }
        for t in removed {
            out.push(t)?;
        }
        Ok(())
    }

    fn saturate(&mut self, cx: &mut RuleCx, store: &mut Counting) -> sparkles_core::Result<bool> {
        if !self.own_saturation {
            return Ok(false);
        }
        let mut inserted = Vec::new();
        let mut queue = Vec::new();
        for t in cx.added {
            if store.insert(*t, Record::EXPLICIT)? {
                inserted.push(*t);
                queue.push(*t);
            }
        }
        let mut sink = Sink::unbounded();
        while let Some(f) = queue.pop() {
            self.inner
                .instances_of(&store.inner, &f, &|x| store.contains(x), &mut sink)?;
            let found: Vec<(Triple, Record)> = sink.drain().collect();
            for (g, r) in found {
                if store.insert(g, r)? {
                    inserted.push(g);
                    queue.push(g);
                }
            }
        }
        self.inserted = inserted;
        Ok(true)
    }

    fn saturated(
        &mut self,
        _cx: &mut RuleCx,
        _store: &Counting,
        out: &mut Sink<'_, Triple, sparkles_core::Error>,
    ) -> sparkles_core::Result<()> {
        for t in std::mem::take(&mut self.inserted) {
            out.push(t)?;
        }
        Ok(())
    }

    fn pass_end(&mut self, _cx: &mut RuleCx) -> sparkles_core::Result<()> {
        self.provable = None;
        self.inserted = Vec::new();
        Ok(())
    }

    fn annotate(
        &mut self,
        _cx: &mut RuleCx,
        _store: &Counting,
        mode: Provenance,
        _f: &Triple,
        _r: &Record,
        _premises: &[&Ann],
    ) -> sparkles_core::Result<Ann> {
        Ok(Ann::derivation(mode, Vec::new()))
    }

    fn tick(&mut self, cx: &mut RuleCx) -> sparkles_core::Result<()> {
        cx.control.check()
    }

    fn save(&self, w: &mut Writer, f: &Triple, r: &Record) -> sparkles_core::Result<()> {
        self.inner.save(w, f, r)
    }

    fn load(&self, r: &mut Reader) -> sparkles_core::Result<(Triple, Record)> {
        self.inner.load(r)
    }
}

/// The snapshots and changes from commit `after` to the store's head.
struct Delta {
    d: DeltaSet,
    added: Vec<Triple>,
    removed: Vec<Triple>,
    control: Control,
}

fn delta(s: &Store, after: u64) -> Delta {
    let control = Control::none();
    let d = DeltaSet::read(s, after, &control);
    assert!(d.complete);
    let (added, removed) = (triples(&d.added), triples(&d.removed));
    Delta {
        d,
        added,
        removed,
        control,
    }
}

fn exact_build(s: &Store, p: Exact) -> FixedPoint<Exact> {
    let mut fp = FixedPoint::new(p);
    let control = Control::none();
    let snap = s.snapshot();
    let mut cx = RuleCx {
        new: snap.clone(),
        old: snap,
        added: &[],
        removed: &[],
        control: &control,
    };
    assert!(
        !fp.program.inner.begin(&cx),
        "no constant moves at the build"
    );
    fp.build(&mut cx, None).expect("build");
    fp
}

/// Advance `fp` from commit `after`, and the consolidated change.
fn exact_advance(
    s: &Store,
    fp: &mut FixedPoint<Exact>,
    after: u64,
) -> sparkles_core::Result<Option<Vec<(Triple, i64)>>> {
    let x = delta(s, after);
    let mut cx = RuleCx {
        new: x.d.snapshot.clone(),
        old: x.d.old().expect("old snapshot"),
        added: &x.added,
        removed: &x.removed,
        control: &x.control,
    };
    if fp.program.inner.begin(&cx) {
        // a rule constant entered the store: build again
        fp.build(&mut cx, None)?;
        return Ok(None);
    }
    let changes = fp.advance(&mut cx, None)?;
    Ok(Some(fixpoint::consolidate(&changes)))
}

fn exact_shown(s: &Store, fp: &FixedPoint<Exact>) -> BTreeSet<String> {
    shown_of(&s.snapshot(), &fp.program.inner, fp.facts())
}

/// The exact mode, with either saturation and several batch lengths, keeps the naive
/// closure through random histories, and its consolidated change is the difference of
/// the closures.
#[test]
fn the_exact_mode_is_maintained_as_its_closure() {
    for (own, batch) in [(false, 1usize), (true, 1), (false, 3), (true, 1024)] {
        for seed in 1..30u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5555);
            let s = Store::in_memory(StoreOptions::default());
            let mut model: BTreeSet<T> = BTreeSet::new();
            let init: Vec<T> = (0..8).map(|_| rng.triple()).collect();
            apply(&s, &[], &init);
            model.extend(init);
            let mut fp = exact_build(&s, Exact::new(RULES, own, batch));
            assert_eq!(exact_shown(&s, &fp), closure(&model), "seed {seed}: build");
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
                let before = exact_shown(&s, &fp);
                let changes = exact_advance(&s, &mut fp, after).expect("advance");
                let want = closure(&model);
                let what = format!("own {own} batch {batch} seed {seed} step {step}");
                assert_eq!(exact_shown(&s, &fp), want, "{what}");
                assert_eq!(
                    fp.store().reserved.get(),
                    0,
                    "{what}: reservations released"
                );
                let Some(changes) = changes else {
                    continue;
                };
                assert!(changes.iter().all(|c| c.1.abs() == 1), "{what}");
                assert_eq!(
                    change_map(&s.snapshot(), &fp.program.inner, &changes),
                    difference(&before, &want),
                    "{what}"
                );
            }
        }
    }
}

const EX: &str = "http://ex/";

fn ex(t: &str) -> String {
    t.split(' ')
        .map(|x| format!("<{EX}{x}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn change(s: &Store, del: &[&str], ins: &[&str]) {
    let mut ops = Vec::new();
    if !del.is_empty() {
        let d: Vec<String> = del.iter().map(|t| format!("{} .", ex(t))).collect();
        ops.push(format!("DELETE DATA {{ {} }}", d.join(" ")));
    }
    if !ins.is_empty() {
        let i: Vec<String> = ins.iter().map(|t| format!("{} .", ex(t))).collect();
        ops.push(format!("INSERT DATA {{ {} }}", i.join(" ")));
    }
    update(s, &ops.join(" ; "), &QueryOptions::default()).expect("update");
}

fn set(ts: &[&str]) -> BTreeSet<String> {
    ts.iter().map(|t| ex(t)).collect()
}

/// Run `steps` of (deletions, insertions, the facts expected after) over `rules` in the
/// exact mode with both saturations and in the one-step mode.
/// Run `steps` with both saturations and the first frontier in either order, against the
/// one-step program and the expected closures, comparing the consolidated changes with
/// the difference of the closures.
fn cases(rules: &str, init: &[&str], steps: &[(&[&str], &[&str], &[&str])]) {
    for own in [false, true] {
        for order in [true, false] {
            let what = format!("own {own} order {order}");
            let s = Store::in_memory(StoreOptions::default());
            change(&s, &[], init);
            let mut p = Exact::new(rules, own, 1);
            p.order = Some(order);
            let mut fp = exact_build(&s, p);
            let mut one = program(rules, &s, Provenance::None, None).expect("one-step build");
            for (i, (del, ins, want)) in steps.iter().enumerate() {
                let before = exact_shown(&s, &fp);
                let after = s.snapshot().commit;
                change(&s, del, ins);
                let ch = exact_advance(&s, &mut fp, after).expect("advance");
                let want = set(want);
                assert_eq!(exact_shown(&s, &fp), want, "{what} step {i}");
                advance(&s, &mut one, after);
                assert_eq!(shown(&s.snapshot(), &one), want, "one-step, step {i}");
                let Some(ch) = ch else {
                    continue;
                };
                assert_eq!(
                    change_map(&s.snapshot(), &fp.program.inner, &ch),
                    difference(&before, &want),
                    "{what} step {i}: {ch:?}"
                );
            }
        }
    }
}

/// A fact that derives itself goes with its explicit support, reported once.
#[test]
fn a_self_supporting_fact_goes_once() {
    cases(
        "[aa: (?x <http://ex/p> ?y) -> (?x <http://ex/p> ?y)]",
        &["a p b"],
        &[(&["a p b"], &[], &[])],
    );
}

/// Facts that derive each other go together when their explicit support goes.
#[test]
fn mutually_recursive_facts_go_together() {
    cases(
        "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]
         [qp: (?x <http://ex/q> ?y) -> (?x <http://ex/p> ?y)]",
        &["a p b"],
        &[(&["a p b"], &[], &[])],
    );
}

/// `A → B`, `B → A`, `C → B`: removing `A` keeps it through `C`, and removing `C`
/// afterwards removes both, which a record of `B` taken from `A` would hide.
#[test]
fn an_exact_proof_leaves_no_cyclic_support() {
    let rules = "[ab: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]
                 [ba: (?x <http://ex/q> ?y) -> (?x <http://ex/p> ?y)]
                 [cb: (?x <http://ex/r> ?y) -> (?x <http://ex/q> ?y)]";
    cases(
        rules,
        &["a p b", "a r b"],
        &[
            (&["a p b"], &[], &["a p b", "a q b", "a r b"]),
            (&["a r b"], &[], &[]),
        ],
    );
}

/// `A ← B`, `B ← A`, `A ← C` with `C` explicit: removing explicit `A` and `B` keeps both
/// with the first frontier in either order (`cases` runs both), since a search blocked on
/// the cycle is not a negative answer.
#[test]
fn a_cycle_blocked_search_is_not_a_negative_answer() {
    let rules = "[ba: (?x <http://ex/q> ?y) -> (?x <http://ex/p> ?y)]
                 [ab: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]
                 [ca: (?x <http://ex/r> ?y) -> (?x <http://ex/p> ?y)]";
    let all: &[&str] = &["a p b", "a q b", "a r b"];
    cases(rules, all, &[(&["a p b", "a q b"], &[], all)]);
}

/// An insertion alone derives new facts; an explicit fact that is also derived stays
/// when it stops being explicit; an inserted fact supports a candidate of the deletion.
#[test]
fn insertions_and_explicit_derived_facts() {
    let rules = "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]
                 [rq: (?x <http://ex/r> ?y) -> (?x <http://ex/q> ?y)]
                 [qs: (?x <http://ex/q> ?y) -> (?x <http://ex/s> ?y)]";
    cases(
        rules,
        &["a p b", "a q b"],
        &[
            (
                &[],
                &["c p d"],
                &["a p b", "a q b", "a s b", "c p d", "c q d", "c s d"],
            ),
            (
                &["a q b"],
                &[],
                &["a p b", "a q b", "a s b", "c p d", "c q d", "c s d"],
            ),
            (
                &["a p b"],
                &["a r b"],
                &["a r b", "a q b", "a s b", "c p d", "c q d", "c s d"],
            ),
            (
                &[],
                &[],
                &["a r b", "a q b", "a s b", "c p d", "c q d", "c s d"],
            ),
        ],
    );
}

/// A sink fails while one fact's consequences are produced, once the store refuses to
/// grow, and the pass leaves no reservation behind.
#[test]
fn a_large_fan_out_fails_while_it_is_produced() {
    let rules = "[f: (?x <http://ex/hub> ?y) (?z <http://ex/m> ?w) -> (?z <http://ex/out> ?w)]";
    let members: Vec<String> = (0..4000).map(|i| format!("i{i} m j{i}")).collect();
    let members: Vec<&str> = members.iter().map(String::as_str).collect();
    for batch in [1usize, 1024] {
        let s = Store::in_memory(StoreOptions::default());
        change(&s, &[], &members);
        let mut fp = exact_build(&s, Exact::new(rules, false, batch));
        assert_eq!(
            fp.store().reserved.get(),
            0,
            "batch {batch}: the build released"
        );
        fp.store_mut().cap = Some(64 << 10);
        fp.store().peak.set(0);
        let after = s.snapshot().commit;
        change(&s, &[], &["a hub b"]);
        fp.store().lookups.set(0);
        let Err(e) = exact_advance(&s, &mut fp, after).map(|c| c.expect("advanced")) else {
            panic!("batch {batch}: the fan-out fits");
        };
        let sparkles_core::Error::BudgetExceeded(b) = e else {
            panic!("batch {batch}: {e:?}");
        };
        assert!(b.requested <= b.limit * 2, "batch {batch}: {b:?}");
        assert!(fp.store().peak.get() <= 64 << 10, "batch {batch}");
        assert_eq!(fp.store().reserved.get(), 0, "batch {batch}");
        // the instances go into the sink as they are found, so the refusal comes before
        // the rule's 4,000 bindings are all enumerated
        let lookups = fp.store().lookups.get();
        assert!(lookups < 2000, "batch {batch}: {lookups} lookups");
    }
}

/// The first frontier is reserved with the operator's other buffers: when the first
/// proof is asked, the frontier of 4,000 removed facts and the table that makes it
/// unique are both charged.
#[test]
fn the_first_frontier_is_reserved() {
    let facts: Vec<String> = (0..4000).map(|i| format!("i{i} m j{i}")).collect();
    let facts: Vec<&str> = facts.iter().map(String::as_str).collect();
    let s = Store::in_memory(StoreOptions::default());
    change(&s, &[], &facts);
    let rules = "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]";
    let mut fp = exact_build(&s, Exact::new(rules, false, 1));
    let after = s.snapshot().commit;
    change(&s, &facts, &[]);
    exact_advance(&s, &mut fp, after).expect("advance");
    assert_eq!(fp.len(), 0);
    let frontier = 4096 * std::mem::size_of::<Triple>() as u64;
    let seen = crate::budget::table_bytes(crate::budget::table_capacity(4000), 24) as u64;
    let at = fp.program.reserved_at_prove.expect("a proof was asked");
    assert!(at >= frontier + seen, "{at} under {}", frontier + seen);
    assert_eq!(fp.store().reserved.get(), 0);
}

/// A one-step build whose instances outgrow `max_bytes` fails as they are produced.
#[test]
fn a_one_step_fan_out_fails_against_max_bytes() {
    let rules = "[f: (?x <http://ex/hub> ?y) (?z <http://ex/m> ?w) -> (?z <http://ex/out> ?w)]";
    let mut members: Vec<String> = (0..4000).map(|i| format!("i{i} m j{i}")).collect();
    members.push("a hub b".into());
    let members: Vec<&str> = members.iter().map(String::as_str).collect();
    let s = Store::in_memory(StoreOptions::default());
    change(&s, &[], &members);
    let full = program(rules, &s, Provenance::None, None).expect("build");
    let limit = full.bytes() * 2 / 3;
    let Err(sparkles_core::Error::BudgetExceeded(b)) =
        program(rules, &s, Provenance::None, Some(limit))
    else {
        panic!("the build fits");
    };
    assert_eq!(b.limit, limit);
}

/// An exact program keeps no annotations, and a one-step program may not saturate by
/// itself.
#[test]
fn the_modes_refuse_what_they_cannot_keep() {
    let s = Store::in_memory(StoreOptions::default());
    change(&s, &[], &["a p b"]);
    let mut fp = FixedPoint::new(Exact::new(
        "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]",
        false,
        1,
    ));
    fp.set_provenance(Provenance::why());
    let control = Control::none();
    let snap = s.snapshot();
    let mut cx = RuleCx {
        new: snap.clone(),
        old: snap,
        added: &[],
        removed: &[],
        control: &control,
    };
    fp.program.inner.begin(&cx);
    assert!(matches!(
        fp.build(&mut cx, None),
        Err(sparkles_core::Error::Invalid(_))
    ));
}

/// The encoding of a fixed point of rules that this release wrote before the store moved
/// into a program's choice still loads, with its annotations.
#[test]
fn an_earlier_checkpoint_still_loads() {
    const DATA: &str =
        "<http://ex/a> <http://ex/p> <http://ex/b> . <http://ex/b> <http://ex/p> <http://ex/c> .";
    const RULES_: &str = "[pq: (?x <http://ex/p> ?y) -> (?x <http://ex/q> ?y)]
     [tr: (?x <http://ex/q> ?y) (?y <http://ex/q> ?z) -> (?x <http://ex/q> ?z)]
     [nw: (?x <http://ex/q> ?y) -> (?x <http://ex/r> <http://ex/new>)]";
    const HEX: &str = include_str!("checkpoint-fixture.hex");
    let bytes: Vec<u8> = (0..HEX.trim().len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&HEX.trim()[i..i + 2], 16).expect("hex"))
        .collect();
    let s = store(DATA);
    let want = program(RULES_, &s, Provenance::why(), None).expect("build");
    let parsed = parse_rules(RULES_).expect("rules");
    let mut back = FixedPoint::new(RuleProgram::new(&parsed).expect("program"));
    back.set_provenance(Provenance::why());
    let snap = s.snapshot();
    RuleProgram::decode(&mut back, &bytes, &snap).expect("decode");
    assert_eq!(shown(&snap, &back), shown(&snap, &want));
    assert_eq!(annotated(&snap, &back), annotated(&snap, &want));
    // and the restored state round-trips through this release's encoding
    let again = RuleProgram::encode(&back, &snap).expect("encode");
    let mut third = FixedPoint::new(RuleProgram::new(&parsed).expect("program"));
    third.set_provenance(Provenance::why());
    RuleProgram::decode(&mut third, &again, &snap).expect("decode");
    assert_eq!(shown(&snap, &third), shown(&snap, &want));
    assert_eq!(annotated(&snap, &third), annotated(&snap, &want));
}

/// Each fact's text with its annotation.
fn annotated(
    snap: &Snapshot,
    fp: &FixedPoint<RuleProgram>,
) -> std::collections::BTreeMap<String, Option<Ann>> {
    fp.facts()
        .into_iter()
        .map(|(f, _)| {
            let text = f
                .iter()
                .map(|&i| fp.program.term(snap, i).expect("a term").to_string())
                .collect::<Vec<_>>()
                .join(" ");
            (text, fp.annotation(&f).cloned())
        })
        .collect()
}
