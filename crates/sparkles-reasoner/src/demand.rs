//! The demand transformation of a compiled rule program (spec F17 §3): magic sets over
//! triples, after Bancilhon, Maier, Sagiv and Ullman (1986) and Beeri and Ramakrishnan
//! (1991).
//!
//! Every atom is a pattern over the one ternary relation of triples. A demand on it is a
//! [`Key`]: which of subject and object are bound, and whether the predicate is a
//! constant, a value bound at run time, or free. A key with a constant predicate splits
//! the relation by that predicate, so `?x rdf:type ex:Person` is the predicate `rdf:type`
//! with the object bound, and its magic facts hold the classes asked for.
//!
//! For each key the transformation makes, from every rule whose head can give a fact of
//! that key, an adorned copy whose body starts with the key's magic atom over the head's
//! terms. Walking the copy's body in the order the rule planner would take with the head's
//! bound variables bound first (sideways information passing), it makes for each body atom
//! that some head can give a magic rule: the atom's demand follows from the copy's magic
//! atom and the atoms and builtins before it. The atom's key joins the work list.
//!
//! Magic facts are triples whose predicate is a blank node local to the run, so the
//! semi-naive engine evaluates the program unchanged and the facts never pass
//! [`Terms::valid`]. A magic fact holds at most two values: a key whose predicate is a
//! value bound at run time with subject and object bound too keeps the subject and the
//! predicate, which asks for more facts than needed and never fewer.
//!
//! A rule whose head has a variable predicate answers a key with a constant predicate
//! through a copy with that variable replaced by the constant.
//!
//! A key may also say that only facts whose subject and object differ are asked for: a
//! body atom whose rule has `notEqual` on its subject and object, or whose rule would
//! only restate one of its own body facts when the atom's fact is reflexive, as in a
//! transitivity rule. Rules whose head can only give reflexive facts, such as
//! `(?c rdfs:subClassOf ?c)`, do not answer such a key, which keeps the reflexive schema
//! rules of RDFS and OWL 2 RL from asking whether every class has an instance.
//!
//! Equality is a lower stage of its own (spec F17 §3.5). When rules derive `owl:sameAs`
//! and read it, the stage evaluates `owl:sameAs` with the free adornment to its fixpoint
//! first, and the stages above read its facts as stored ones: their `owl:sameAs` atoms ask
//! for nothing and the rules that derive `owl:sameAs` are left out of them. Without it,
//! every value a query touches would ask what it equals, and the `eq-rep-*` rules would
//! ask for every fact of every value.
//!
//! `noValue` reads facts that must be complete before the rule runs. The pattern it reads,
//! with its constants bound and its variables free, is a demand of a lower stage with
//! magic predicates of its own, evaluated to its fixpoint first. A program whose stages
//! do not end is not stratified and is refused.

use crate::budget::MemoryExceeded;
use crate::builtins::{BuiltinKind, HeadAction};
use crate::engine::{Atom, CBuiltin, CHead, CRule, Slot, Step};
use crate::graph::Triple;
use crate::terms::Terms;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{BTreeSet, VecDeque};

/// The most rules a demand program may have, over all its stages. A program past it is
/// refused, since its evaluation would plan that many rules in every iteration.
pub(crate) const MAX_RULES: usize = 50_000;

/// How a key binds the predicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum KeyP {
    Free,
    /// a value bound at run time, held in the magic fact
    Bound,
    /// a constant of the program or the query
    Const(u64),
}

/// A demand: which positions of the facts asked for are bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Key {
    pub s: bool,
    pub p: KeyP,
    pub o: bool,
    /// only facts whose subject and object differ are asked for
    pub neq: bool,
}

impl Key {
    pub fn new(s: bool, p: KeyP, o: bool) -> Key {
        // a magic fact holds two values: subject and predicate, when it would need three
        let o = o && !(s && p == KeyP::Bound);
        Key {
            s,
            p,
            o,
            neq: false,
        }
    }

    fn with_neq(self, neq: bool) -> Key {
        Key { neq, ..self }
    }

    /// The adornment, `b` for a bound position and `f` for a free one, in the order
    /// subject, predicate, object (a constant predicate counts as bound).
    pub fn adornment(&self) -> String {
        let c = |b: bool| if b { 'b' } else { 'f' };
        [c(self.s), c(self.p != KeyP::Free), c(self.o)]
            .into_iter()
            .collect()
    }
}

/// The key of a pattern whose positions `bound` says are bound: constants, and variables
/// bound before it.
fn key_of(a: &Atom, bound: &[bool]) -> Key {
    let b = |s: Slot| match s {
        Slot::Const(_) => true,
        Slot::Var(v) => bound[v],
        Slot::Any => false,
    };
    let p = match a.p {
        Slot::Const(c) => KeyP::Const(c),
        Slot::Var(v) if bound[v] => KeyP::Bound,
        _ => KeyP::Free,
    };
    Key::new(b(a.s), p, b(a.o))
}

/// Whether body atom `i` of `body` needs only facts whose subject and object differ: the
/// rule tests `notEqual` on them, or with the object equal to the subject the head of the
/// copy (`heads`) is another body atom, so the instance restates a fact it matched.
fn irreflexive(body: &CRule, i: usize, heads: &[CHead]) -> bool {
    let a = &body.atoms[i];
    let (x, y) = (a.s, a.o);
    // the variable that equal subject and object would replace, and its replacement
    let (from, to) = match (x, y) {
        _ if x == y => return false,
        (_, Slot::Var(v)) if x != Slot::Any => (v, x),
        (Slot::Var(v), _) if y != Slot::Any => (v, y),
        (Slot::Const(_), Slot::Const(_)) => return false,
        _ => return false,
    };
    let pair = [x, y];
    if body.builtins.iter().any(|b| {
        b.kind == BuiltinKind::NotEqual
            && b.args.len() == 2
            && pair.contains(&b.args[0])
            && pair.contains(&b.args[1])
            && b.args[0] != b.args[1]
    }) {
        return true;
    }
    let [CHead::Triple(hs, hp, ho)] = heads else {
        return false;
    };
    let f = |s: Slot| if s == Slot::Var(from) { to } else { s };
    let head = (f(*hs), f(*hp), f(*ho));
    body.atoms
        .iter()
        .enumerate()
        .any(|(j, b)| j != i && (f(b.s), f(b.p), f(b.o)) == head)
}

/// Whether a head can only give facts whose subject and object are the same.
fn reflexive_head(hs: (Slot, Slot, Slot)) -> bool {
    match (hs.0, hs.2) {
        (Slot::Var(a), Slot::Var(b)) => a == b,
        (Slot::Const(a), Slot::Const(b)) => a == b,
        _ => false,
    }
}

/// The terms a rule head gives: a triple's, or for a head action the positions it fixes
/// (`Any` where it puts the members of a list).
fn head_slots(h: &CHead) -> (Slot, Slot, Slot) {
    match h {
        CHead::Triple(s, p, o) => (*s, *p, *o),
        CHead::Action(HeadAction::ListMapAsSubject, a) => (Slot::Any, a[1], a[2]),
        CHead::Action(HeadAction::ListMapAsObject, a) => (a[0], a[1], Slot::Any),
    }
}

/// Whether a fact that the head gives can match the pattern.
fn unifies(a: (Slot, Slot, Slot), h: (Slot, Slot, Slot)) -> bool {
    let one = |x: Slot, y: Slot| !matches!((x, y), (Slot::Const(c), Slot::Const(d)) if c != d);
    one(a.0, h.0) && one(a.1, h.1) && one(a.2, h.2)
}

/// Whether rules can give facts that match the pattern `a`.
pub(crate) fn intensional(rules: &[CRule], a: (Slot, Slot, Slot)) -> bool {
    rules
        .iter()
        .flat_map(|r| r.head.iter())
        .any(|h| unifies(a, head_slots(h)))
}

fn clone_builtin(b: &CBuiltin) -> CBuiltin {
    CBuiltin {
        kind: b.kind,
        args: b.args.clone(),
        inputs: b.inputs.clone(),
        outputs: b.outputs.clone(),
        regex: b.regex.clone(),
    }
}

fn clone_head(h: &CHead) -> CHead {
    match h {
        CHead::Triple(s, p, o) => CHead::Triple(*s, *p, *o),
        CHead::Action(a, args) => CHead::Action(*a, args.clone()),
    }
}

/// `r` with variable `v` replaced by the constant `c` everywhere. A builtin that bound
/// `v` then compares its output with `c`.
fn specialize(r: &CRule, v: usize, c: u64) -> CRule {
    let sub = |s: Slot| if s == Slot::Var(v) { Slot::Const(c) } else { s };
    CRule {
        name: r.name.clone(),
        nvars: r.nvars,
        atoms: r
            .atoms
            .iter()
            .map(|a| Atom {
                s: sub(a.s),
                p: sub(a.p),
                o: sub(a.o),
            })
            .collect(),
        builtins: r
            .builtins
            .iter()
            .map(|b| CBuiltin {
                kind: b.kind,
                args: b.args.iter().map(|&s| sub(s)).collect(),
                inputs: b.inputs.iter().copied().filter(|&x| x != v).collect(),
                outputs: b.outputs.iter().copied().filter(|&x| x != v).collect(),
                regex: b.regex.clone(),
            })
            .collect(),
        head: r
            .head
            .iter()
            .map(|h| match h {
                CHead::Triple(s, p, o) => CHead::Triple(sub(*s), sub(*p), sub(*o)),
                CHead::Action(a, args) => CHead::Action(*a, args.iter().map(|&s| sub(s)).collect()),
            })
            .collect(),
    }
}

/// One stage of a demand program: its rules and the magic predicates of its keys.
pub(crate) struct Stage {
    pub rules: Vec<CRule>,
    magic: FxHashMap<Key, u64>,
    /// the original rules each key's adorned copies come from, and the keys their bodies
    /// ask for: the dependency cone of a key, for EXPLAIN
    copies: FxHashMap<Key, (BTreeSet<String>, BTreeSet<Key>)>,
    /// the magic facts the stage starts from (the lower stages': `noValue` patterns)
    pub seeds: Vec<Triple>,
    /// evaluate the stage only when a fact with this predicate holds
    pub only_if: Option<u64>,
}

impl Stage {
    pub fn magic(&self, k: &Key) -> Option<u64> {
        self.magic.get(k).copied()
    }

    /// Each key with its magic predicate.
    pub fn keys(&self) -> impl Iterator<Item = (&Key, u64)> {
        self.magic.iter().map(|(k, m)| (k, *m))
    }

    /// The original rules in the dependency cone of `k`.
    pub fn cone(&self, k: &Key) -> BTreeSet<String> {
        let mut seen: FxHashSet<Key> = FxHashSet::default();
        let mut out = BTreeSet::new();
        let mut todo = vec![*k];
        while let Some(k) = todo.pop() {
            if !seen.insert(k) {
                continue;
            }
            if let Some((rules, next)) = self.copies.get(&k) {
                out.extend(rules.iter().cloned());
                todo.extend(next.iter().copied());
            }
        }
        out
    }
}

/// A demand program: the query's stage first, then the lower stages that its `noValue`
/// builtins need, each evaluated before the one above it.
pub(crate) struct Program {
    pub stages: Vec<Stage>,
    /// every magic predicate of every stage
    pub magic_preds: FxHashSet<u64>,
    /// the value of the positions a magic fact does not hold
    pub dummy: u64,
}

/// The magic fact of key `k` in a stage whose magic predicate is `m`, for the values
/// `s`, `p` and `o` of the positions the key binds (the others are ignored).
pub(crate) fn magic_fact(k: &Key, m: u64, dummy: u64, s: u64, p: u64, o: u64) -> Triple {
    let vals: Vec<u64> = match k.p {
        KeyP::Bound => [k.s.then_some(s), Some(p), k.o.then_some(o)]
            .into_iter()
            .flatten()
            .collect(),
        _ => [k.s.then_some(s), k.o.then_some(o)]
            .into_iter()
            .flatten()
            .collect(),
    };
    match (k.p, k.s, k.o) {
        // subject and object keep their places, so a magic atom reads like its pattern
        (KeyP::Free | KeyP::Const(_), _, _) => {
            [if k.s { s } else { dummy }, m, if k.o { o } else { dummy }]
        }
        _ => [
            vals.first().copied().unwrap_or(dummy),
            m,
            vals.get(1).copied().unwrap_or(dummy),
        ],
    }
}

/// The magic atom of key `k` over the slots `(s, p, o)`, with the predicate `m`.
fn magic_atom(k: &Key, m: u64, dummy: u64, (s, p, o): (Slot, Slot, Slot)) -> Atom {
    let d = Slot::Const(dummy);
    match k.p {
        KeyP::Bound => {
            let vals: Vec<Slot> = [k.s.then_some(s), Some(p), k.o.then_some(o)]
                .into_iter()
                .flatten()
                .collect();
            Atom {
                s: vals.first().copied().unwrap_or(d),
                p: Slot::Const(m),
                o: vals.get(1).copied().unwrap_or(d),
            }
        }
        _ => Atom {
            s: if k.s { s } else { d },
            p: Slot::Const(m),
            o: if k.o { o } else { d },
        },
    }
}

/// A `noValue` builtin's pattern as a demand of the stage below: its constants bound, its
/// variables free, with the constants' values.
fn no_value_demand(b: &CBuiltin) -> (Key, [u64; 3]) {
    let c = |s: Option<&Slot>| match s {
        Some(Slot::Const(c)) => Some(*c),
        _ => None,
    };
    let (s, p, o) = (c(b.args.first()), c(b.args.get(1)), c(b.args.get(2)));
    let key = Key::new(s.is_some(), p.map_or(KeyP::Free, KeyP::Const), o.is_some());
    (key, [s.unwrap_or(0), p.unwrap_or(0), o.unwrap_or(0)])
}

/// An estimate of the facts an atom matches with the variables `bound` bound: from the
/// stored facts, and for a pattern of unknown predicate (what rules may derive).
pub(crate) type Estimate<'a> = &'a dyn Fn(&Atom, &[bool]) -> (f64, f64);

/// The order in which a rule copy's body is walked to pass bindings sideways: builtins as
/// soon as their inputs are bound, then the atom with the fewest estimated matches. An
/// atom that rules derive is estimated at least as a pattern of unknown predicate, since
/// its stored facts say nothing of its derived ones, and comes after every other when
/// its demand would be wide: a free predicate, or `rdf:type` with a free class. Such a
/// demand asks for every fact or every type of each value, which asks in turn for the
/// facts of their neighbours, so placing it after the atoms that bind its predicate or
/// class keeps the demand from spreading over the graph. `None` when a builtin's inputs
/// are never bound.
fn sip(
    rule: &CRule,
    prebound: &[usize],
    est: Estimate<'_>,
    derived: &dyn Fn(&Atom) -> bool,
    rdf_type: u64,
) -> Option<Vec<Step>> {
    let mut bound = vec![false; rule.nvars];
    for &v in prebound {
        bound[v] = true;
    }
    let mut steps = Vec::with_capacity(rule.atoms.len() + rule.builtins.len());
    let mut atoms_left: Vec<usize> = (0..rule.atoms.len()).collect();
    let mut bi_left: Vec<usize> = (0..rule.builtins.len()).collect();
    loop {
        let mut progress = true;
        while progress {
            progress = false;
            bi_left.retain(|&j| {
                let b = &rule.builtins[j];
                if b.inputs.iter().all(|&v| bound[v]) {
                    steps.push(Step::Builtin(j));
                    for &v in &b.outputs {
                        bound[v] = true;
                    }
                    progress = true;
                    false
                } else {
                    true
                }
            });
        }
        if atoms_left.is_empty() {
            break;
        }
        let cost = |i: usize| {
            let a = &rule.atoms[i];
            let (stored, unknown) = est(a, &bound);
            if !derived(a) {
                return stored;
            }
            let b = |s: Slot| match s {
                Slot::Const(_) => true,
                Slot::Var(v) => bound[v],
                Slot::Any => false,
            };
            let wide = !b(a.p) || (a.p == Slot::Const(rdf_type) && !b(a.o));
            let c = stored.max(unknown);
            if wide { c * 1e9 } else { c }
        };
        let best = (0..atoms_left.len())
            .min_by(|&x, &y| cost(atoms_left[x]).total_cmp(&cost(atoms_left[y])))
            .expect("atoms left");
        let i = atoms_left.remove(best);
        let a = &rule.atoms[i];
        for s in [a.s, a.p, a.o] {
            if let Slot::Var(v) = s {
                bound[v] = true;
            }
        }
        steps.push(Step::Atom {
            atom: i,
            lo: 0,
            hi: 0,
        });
    }
    bi_left.is_empty().then_some(steps)
}

/// The schema vocabulary of a program: the RDFS and OWL predicates its rules name, apart
/// from equality and annotations, and the RDF, RDFS and OWL classes of properties and
/// classes it names as types.
pub(crate) struct Vocab {
    schema: FxHashSet<u64>,
    meta: FxHashSet<u64>,
}

impl Vocab {
    fn of(rules: &[CRule], terms: &Terms) -> Vocab {
        const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
        const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
        const OWL: &str = "http://www.w3.org/2002/07/owl#";
        let iri = |c: u64| match terms.term(c) {
            Some(oxrdf::Term::NamedNode(n)) => Some(n.into_string()),
            _ => None,
        };
        let is_schema = |i: &str| {
            matches!(
                i.strip_prefix(RDFS),
                Some("subClassOf" | "subPropertyOf" | "domain" | "range")
            ) || i.strip_prefix(OWL).is_some_and(|l| {
                !matches!(
                    l,
                    "sameAs"
                        | "differentFrom"
                        | "versionInfo"
                        | "priorVersion"
                        | "backwardCompatibleWith"
                        | "incompatibleWith"
                        | "deprecated"
                )
            })
        };
        let is_meta = |i: &str| {
            [RDF, RDFS, OWL].iter().any(|ns| {
                i.strip_prefix(ns).is_some_and(|l| {
                    l.ends_with("Property")
                        || matches!(l, "Class" | "Datatype" | "Restriction" | "Ontology")
                })
            })
        };
        let rdf_type = format!("{RDF}type");
        let mut v = Vocab {
            schema: FxHashSet::default(),
            meta: FxHashSet::default(),
        };
        let mut see = |p: Slot, o: Slot| {
            let Slot::Const(p) = p else { return };
            let Some(pi) = iri(p) else { return };
            if is_schema(&pi) {
                v.schema.insert(p);
            } else if pi == rdf_type
                && let Slot::Const(c) = o
                && iri(c).is_some_and(|ci| is_meta(&ci))
            {
                v.meta.insert(c);
            }
        };
        for r in rules {
            for a in &r.atoms {
                see(a.p, a.o);
            }
            for h in &r.head {
                let (_, p, o) = head_slots(h);
                see(p, o);
            }
        }
        v
    }
}

/// How a body atom that rules give facts for is asked for.
struct StageBuilder<'a> {
    est: Estimate<'a>,
    rdf_type: u64,
    vocab: &'a Vocab,
    rules: &'a [CRule],
    heads: Vec<(Slot, Slot, Slot)>,
    /// a predicate a lower stage derives in full, whose atoms ask for nothing here
    complete: Option<u64>,
    terms: &'a Terms,
    dummy: u64,
    stage: Stage,
    todo: VecDeque<Key>,
    magic_preds: &'a mut FxHashSet<u64>,
    below: Vec<(Key, Option<[u64; 3]>)>,
    below_seen: FxHashSet<(Key, [u64; 3])>,
}

impl StageBuilder<'_> {
    fn fresh(&self) -> Result<u64, TransformError> {
        self.terms.try_fresh_bnode().map_err(TransformError::Memory)
    }

    /// Whether every head of a copy gives schema facts: a predicate of the RDFS and OWL
    /// vocabularies, or `rdf:type` with an RDF, RDFS or OWL class of properties and classes.
    fn schema_head(&self, heads: &[CHead]) -> bool {
        heads.iter().all(|h| match h {
            CHead::Triple(_, Slot::Const(p), o) => {
                self.vocab.schema.contains(p)
                    || (*p == self.rdf_type
                        && matches!(o, Slot::Const(c) if self.vocab.meta.contains(c)))
            }
            _ => false,
        })
    }

    /// Whether a body atom of a copy asks for derived facts. A copy that gives schema facts
    /// reads instance facts as they are and asks only for schema facts and types, so the
    /// schema never asks for the instances of a class or the facts of a property: which
    /// classes have instances (`rdf:type rdfs:range rdfs:Class`) or which properties are
    /// used (`rdf1`) is read from the facts at hand.
    fn asks(&self, a: &Atom, schema_copy: bool) -> bool {
        if !self.intensional(a) {
            return false;
        }
        if !schema_copy {
            return true;
        }
        match a.p {
            Slot::Const(p) if self.vocab.schema.contains(&p) => true,
            Slot::Const(p) if p == self.rdf_type => match a.o {
                Slot::Const(c) => self.vocab.meta.contains(&c),
                Slot::Var(_) => true,
                Slot::Any => false,
            },
            _ => false,
        }
    }

    fn intensional(&self, a: &Atom) -> bool {
        if self.complete.is_some_and(|c| a.p == Slot::Const(c)) {
            return false;
        }
        self.heads.iter().any(|h| unifies((a.s, a.p, a.o), *h))
    }

    fn magic_of(&mut self, k: Key) -> Result<u64, TransformError> {
        if let Some(&m) = self.stage.magic.get(&k) {
            return Ok(m);
        }
        let m = self.fresh()?;
        self.stage.magic.insert(k, m);
        self.magic_preds.insert(m);
        self.todo.push_back(k);
        Ok(m)
    }

    /// Add the rule `r` for key `k` behind `guard`, whose variables `prebound` are bound,
    /// without the body atom `skip`, with `head` for its head and `nvars` variables, and
    /// the magic rules of its body atoms.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        k: Key,
        r: &CRule,
        guard: Atom,
        prebound: &[usize],
        skip: Option<usize>,
        head: Vec<CHead>,
        nvars: usize,
    ) -> Result<(), TransformError> {
        let mut body = r.clone_shallow();
        body.nvars = nvars;
        if let Some(i) = skip {
            body.atoms.remove(i);
        }
        let schema_copy = self.schema_head(&head);
        let Some(steps) = sip(
            &body,
            prebound,
            self.est,
            &|a| self.asks(a, schema_copy),
            self.rdf_type,
        ) else {
            return Ok(());
        };
        let m = guard.p;
        self.stage
            .copies
            .entry(k)
            .or_default()
            .0
            .insert(r.name.clone());
        let mut bound = vec![false; nvars];
        for &v in prebound {
            bound[v] = true;
        }
        let mut before_atoms: Vec<usize> = Vec::new();
        let mut before_builtins: Vec<usize> = Vec::new();
        let mut asks: Vec<Key> = Vec::new();
        for st in &steps {
            match *st {
                Step::Atom { atom, .. } => {
                    let a = body.atoms[atom].clone();
                    if self.asks(&a, schema_copy) {
                        let ka = key_of(&a, &bound).with_neq(irreflexive(&body, atom, &head));
                        let ma = self.magic_of(ka)?;
                        asks.push(ka);
                        let mh = magic_atom(&ka, ma, self.dummy, (a.s, a.p, a.o));
                        // the demand the guard already is
                        let trivial = before_atoms.is_empty()
                            && before_builtins.is_empty()
                            && Slot::Const(ma) == m
                            && mh.s == guard.s
                            && mh.o == guard.o;
                        if !trivial {
                            let mut atoms = vec![guard.clone()];
                            atoms.extend(before_atoms.iter().map(|&i| body.atoms[i].clone()));
                            self.stage.rules.push(CRule {
                                name: format!("{} (demand)", r.name),
                                nvars,
                                atoms,
                                builtins: before_builtins
                                    .iter()
                                    .map(|&j| clone_builtin(&body.builtins[j]))
                                    .collect(),
                                head: vec![CHead::Triple(mh.s, mh.p, mh.o)],
                            });
                        }
                    }
                    for s in [a.s, a.p, a.o] {
                        if let Slot::Var(v) = s {
                            bound[v] = true;
                        }
                    }
                    before_atoms.push(atom);
                }
                Step::Builtin(j) => {
                    for &v in &body.builtins[j].outputs {
                        bound[v] = true;
                    }
                    before_builtins.push(j);
                }
            }
        }
        if tracing::enabled!(target: "sparkles_reasoner::demand", tracing::Level::DEBUG) {
            let show = |k: &Key| {
                let p = match k.p {
                    KeyP::Const(c) => self
                        .terms
                        .term(c)
                        .map_or_else(|| format!("#{c}"), |t| t.to_string()),
                    p => format!("{p:?}"),
                };
                format!("{p}^{}{}", k.adornment(), if k.neq { "!" } else { "" })
            };
            let order: Vec<String> = steps
                .iter()
                .filter_map(|st| match st {
                    Step::Atom { atom, .. } => Some(format!("{}", atom)),
                    Step::Builtin(_) => None,
                })
                .collect();
            tracing::debug!(
                target: "sparkles_reasoner::demand",
                "copy {} via {} order [{}] asks [{}]",
                show(&k),
                r.name,
                order.join(" "),
                asks.iter().map(show).collect::<Vec<_>>().join(", ")
            );
        }
        self.stage.copies.entry(k).or_default().1.extend(asks);
        for b in &body.builtins {
            if b.kind == BuiltinKind::NoValue {
                let (nk, vals) = no_value_demand(b);
                if self.below_seen.insert((nk, vals)) {
                    self.below.push((nk, Some(vals)));
                }
            }
        }
        let mut atoms = vec![guard];
        atoms.extend(body.atoms);
        self.stage.rules.push(CRule {
            name: r.name.clone(),
            nvars,
            atoms,
            builtins: body.builtins,
            head,
        });
        Ok(())
    }

    /// The adorned copies of every rule that can give facts of key `k`.
    fn adorn(&mut self, k: Key) -> Result<(), TransformError> {
        let m = self.stage.magic[&k];
        if self.linear(k, m)? {
            return Ok(());
        }
        let rules = self.rules;
        let mut made: FxHashSet<(usize, usize)> = FxHashSet::default();
        for (ri, r) in rules.iter().enumerate() {
            for (hi, h) in r.head.iter().enumerate() {
                let hs = head_slots(h);
                if k.neq && reflexive_head(hs) {
                    continue;
                }
                // a constant predicate asked for: the head must give it
                let spec = match (k.p, hs.1) {
                    (KeyP::Const(c), Slot::Const(d)) if c != d => continue,
                    (KeyP::Const(c), Slot::Var(v)) => Some((v, c)),
                    _ => None,
                };
                if !made.insert((ri, hi)) {
                    continue;
                }
                let owned;
                let r: &CRule = match spec {
                    Some((v, c)) => {
                        owned = specialize(r, v, c);
                        &owned
                    }
                    None => r,
                };
                let hs = head_slots(&r.head[hi]);
                // the head's variables in the positions the key binds
                let mut prebound = Vec::new();
                let mut bind = |s: Slot, on: bool| {
                    if let (true, Slot::Var(v)) = (on, s) {
                        prebound.push(v);
                    }
                };
                bind(hs.0, k.s);
                bind(hs.1, k.p == KeyP::Bound);
                bind(hs.2, k.o);
                let guard = magic_atom(&k, m, self.dummy, hs);
                let head = r.head.iter().map(clone_head).collect();
                self.emit(k, r, guard, &prebound, None, head, r.nvars)?;
            }
        }
        Ok(())
    }

    /// Linear recursion (Naughton, Ramakrishnan, Sagiv and Ullman, "Efficient evaluation
    /// of right-, left-, and multi-linear rules", SIGMOD 1989). For a key with a constant
    /// predicate `P` and one end bound, when every rule for `P` is an exit rule (no body
    /// atom can be a `P` fact) or linear in the bound end (one body atom is a `P` fact,
    /// whose free end is the head's free end and appears nowhere else), the facts asked
    /// for are those that exit rules give at the nodes the linear rules reach from each
    /// bound value. The rewriting keeps, for each value asked for, the nodes reached as
    /// facts `(value, L, node)` and gives the answers for the value itself, so a chain of
    /// `n` nodes derives `n` facts per value instead of the `n²` of the adorned copies.
    /// Returns whether it applied.
    fn linear(&mut self, k: Key, m: u64) -> Result<bool, TransformError> {
        let KeyP::Const(p) = k.p else {
            return Ok(false);
        };
        // the bound end: the subject (right-linear rules) or the object (left-linear)
        let subject = match (k.s, k.o) {
            (true, false) => true,
            (false, true) => false,
            _ => return Ok(false),
        };
        let end = |a: &Atom| if subject { a.s } else { a.o };
        let other = |a: &Atom| if subject { a.o } else { a.s };
        struct Plan {
            rule: CRule,
            rec: Option<usize>,
        }
        let mut plans: Vec<Plan> = Vec::new();
        let rules = self.rules;
        for r in rules {
            for (hi, h) in r.head.iter().enumerate() {
                let hs = head_slots(h);
                let spec = match hs.1 {
                    Slot::Const(d) if d != p => continue,
                    Slot::Var(v) => Some(v),
                    Slot::Const(_) => None,
                    Slot::Any => return Ok(false),
                };
                if r.head.len() != 1 || !matches!(h, CHead::Triple(..)) {
                    return Ok(false);
                }
                let r = match spec {
                    Some(v) => specialize(r, v, p),
                    None => r.clone_shallow(),
                };
                let (hs, _, ho) = head_slots(&r.head[hi]);
                let head = Atom {
                    s: hs,
                    p: Slot::Const(p),
                    o: ho,
                };
                if k.neq && reflexive_head((head.s, head.p, head.o)) {
                    continue;
                }
                let (Slot::Var(he), Slot::Var(hf)) = (end(&head), other(&head)) else {
                    return Ok(false);
                };
                if he == hf {
                    return Ok(false);
                }
                let recs: Vec<usize> = (0..r.atoms.len())
                    .filter(|&i| {
                        let a = &r.atoms[i];
                        a.p == Slot::Const(p) || matches!(a.p, Slot::Var(_) | Slot::Any)
                    })
                    .collect();
                let rec =
                    match recs[..] {
                        [] => None,
                        [i] => {
                            let a = &r.atoms[i];
                            // the free end passes through unchanged and appears nowhere else
                            let passes = a.p == Slot::Const(p) && other(a) == Slot::Var(hf);
                            let elsewhere =
                                r.atoms.iter().enumerate().any(|(j, b)| {
                                    j != i && [b.s, b.p, b.o].contains(&Slot::Var(hf))
                                }) || r.builtins.iter().any(|b| b.args.contains(&Slot::Var(hf)))
                                    || end(a) == Slot::Var(hf)
                                    || a.p == Slot::Var(hf);
                            // the node it reaches must be bound without it
                            let reached = match end(a) {
                                Slot::Var(y) => {
                                    y == he
                                        || r.atoms.iter().enumerate().any(|(j, b)| {
                                            j != i && [b.s, b.p, b.o].contains(&Slot::Var(y))
                                        })
                                        || r.builtins.iter().any(|b| b.outputs.contains(&y))
                                }
                                Slot::Const(_) => true,
                                Slot::Any => false,
                            };
                            if !passes || elsewhere || !reached {
                                return Ok(false);
                            }
                            Some(i)
                        }
                        _ => return Ok(false),
                    };
                plans.push(Plan { rule: r, rec });
            }
        }
        if !plans.iter().any(|pl| pl.rec.is_some()) {
            return Ok(false);
        }
        let reach = self.fresh()?;
        self.magic_preds.insert(reach);
        let d = Slot::Const(self.dummy);
        // every value asked for reaches itself
        let start = if subject {
            Atom {
                s: Slot::Var(0),
                p: Slot::Const(m),
                o: d,
            }
        } else {
            Atom {
                s: d,
                p: Slot::Const(m),
                o: Slot::Var(0),
            }
        };
        self.stage.rules.push(CRule {
            name: "linear recursion (demand)".into(),
            nvars: 1,
            atoms: vec![start],
            builtins: Vec::new(),
            head: vec![CHead::Triple(
                Slot::Var(0),
                Slot::Const(reach),
                Slot::Var(0),
            )],
        });
        for pl in plans {
            let r = &pl.rule;
            let (hs, _, ho) = head_slots(&r.head[0]);
            let head_end = if subject { hs } else { ho };
            let Slot::Var(he) = head_end else {
                unreachable!("checked above")
            };
            // the value asked for, as a new variable
            let c = r.nvars;
            let guard = Atom {
                s: Slot::Var(c),
                p: Slot::Const(reach),
                o: head_end,
            };
            let prebound = [c, he];
            match pl.rec {
                Some(i) => {
                    let next = end(&r.atoms[i]);
                    let head = vec![CHead::Triple(Slot::Var(c), Slot::Const(reach), next)];
                    self.emit(k, r, guard, &prebound, Some(i), head, c + 1)?;
                }
                None => {
                    let head = if subject {
                        CHead::Triple(Slot::Var(c), Slot::Const(p), ho)
                    } else {
                        CHead::Triple(hs, Slot::Const(p), Slot::Var(c))
                    };
                    self.emit(k, r, guard, &prebound, None, vec![head], c + 1)?;
                }
            }
        }
        Ok(true)
    }
}

impl CRule {
    /// A copy of the rule (its compiled regular expressions are shared).
    pub(crate) fn clone_shallow(&self) -> CRule {
        CRule {
            name: self.name.clone(),
            nvars: self.nvars,
            atoms: self.atoms.clone(),
            builtins: self.builtins.iter().map(clone_builtin).collect(),
            head: self.head.iter().map(clone_head).collect(),
        }
    }
}

/// Make the demand program of `rules` for the query's keys `roots`. Magic predicates and
/// the dummy value are fresh blank nodes of `terms`.
pub(crate) fn transform(
    rules: &[CRule],
    terms: &Terms,
    roots: &[Key],
    est: Estimate<'_>,
) -> Result<Program, TransformError> {
    let dummy = terms.try_fresh_bnode().map_err(TransformError::Memory)?;
    let rdf_type = terms
        .try_id_for(&oxrdf::Term::NamedNode(
            oxrdf::vocab::rdf::TYPE.into_owned(),
        ))
        .map_err(TransformError::Memory)?;
    let vocab = Vocab::of(rules, terms);
    // equality as a lower stage, when rules derive owl:sameAs and read it
    let same_as = terms
        .try_id_for(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            "http://www.w3.org/2002/07/owl#sameAs",
        )))
        .map_err(TransformError::Memory)?;
    let derives = |r: &CRule| {
        r.head
            .iter()
            .any(|h| head_slots(h).1 == Slot::Const(same_as))
    };
    let eq = rules.iter().any(derives)
        && rules
            .iter()
            .any(|r| r.atoms.iter().any(|a| a.p == Slot::Const(same_as)));
    // the rules of the stages above equality: without those that only derive owl:sameAs
    let upper: Vec<CRule> = if eq {
        rules
            .iter()
            .filter(|r| {
                !r.head
                    .iter()
                    .all(|h| head_slots(h).1 == Slot::Const(same_as))
            })
            .map(CRule::clone_shallow)
            .collect()
    } else {
        Vec::new()
    };
    let heads_of = |rs: &[CRule]| -> Vec<(Slot, Slot, Slot)> {
        rs.iter()
            .flat_map(|r| r.head.iter().map(head_slots))
            .collect()
    };
    // the first pass of equality: without the rules that read owl:sameAs to derive other
    // facts (the eq-rep-* rules), which can only fire once owl:sameAs holds a fact
    let first: Vec<CRule> = if eq {
        rules
            .iter()
            .filter(|r| derives(r) || !r.atoms.iter().any(|a| a.p == Slot::Const(same_as)))
            .map(CRule::clone_shallow)
            .collect()
    } else {
        Vec::new()
    };
    let all_heads = heads_of(rules);
    let upper_heads = heads_of(&upper);
    let first_heads = heads_of(&first);
    // 0: the query's stages, 1: equality in full, 2: equality's first pass
    let mut phase = 0;
    // the noValue builtins of the program, for the stratification check
    let no_values: usize = rules
        .iter()
        .map(|r| {
            r.builtins
                .iter()
                .filter(|b| b.kind == BuiltinKind::NoValue)
                .count()
        })
        .sum();
    let mut stages: Vec<Stage> = Vec::new();
    let mut magic_preds = FxHashSet::default();
    let mut total = 0usize;
    let mut level: Vec<(Key, Option<[u64; 3]>)> = roots.iter().map(|k| (*k, None)).collect();
    loop {
        let mut only_if = None;
        if level.is_empty() {
            if !eq || phase == 2 {
                break;
            }
            // the equality stages, below every other: the full one runs only when the
            // first pass, evaluated before it, found owl:sameAs facts
            phase += 1;
            if phase == 1 {
                only_if = Some(same_as);
            }
            level = vec![(Key::new(false, KeyP::Const(same_as), false), Some([0; 3]))];
        }
        if stages.len() > no_values + 2 * eq as usize {
            return Err(TransformError::NotStratified);
        }
        let (stage_rules, heads, complete): (&[CRule], &[(Slot, Slot, Slot)], _) = match phase {
            0 if eq => (&upper, &upper_heads, Some(same_as)),
            0 | 1 => (rules, &all_heads, None),
            _ => (&first, &first_heads, None),
        };
        let mut b = StageBuilder {
            est,
            rdf_type,
            vocab: &vocab,
            rules: stage_rules,
            heads: heads.to_vec(),
            complete,
            terms,
            dummy,
            stage: Stage {
                rules: Vec::new(),
                magic: FxHashMap::default(),
                copies: FxHashMap::default(),
                seeds: Vec::new(),
                only_if,
            },
            todo: VecDeque::new(),
            magic_preds: &mut magic_preds,
            below: Vec::new(),
            below_seen: FxHashSet::default(),
        };
        let mut seeds: Vec<(Key, [u64; 3])> = Vec::new();
        for (k, vals) in &level {
            b.magic_of(*k)?;
            if let Some(v) = vals {
                seeds.push((*k, *v));
            }
        }
        let mut done: FxHashSet<Key> = FxHashSet::default();
        while let Some(k) = b.todo.pop_front() {
            if !done.insert(k) {
                continue;
            }
            b.adorn(k)?;
            if total + b.stage.rules.len() > MAX_RULES {
                return Err(TransformError::TooLarge(total + b.stage.rules.len()));
            }
        }
        let StageBuilder {
            mut stage, below, ..
        } = b;
        for (k, v) in seeds {
            let m = stage.magic[&k];
            stage.seeds.push(magic_fact(&k, m, dummy, v[0], v[1], v[2]));
        }
        total += stage.rules.len();
        stages.push(stage);
        level = below;
    }
    Ok(Program {
        stages,
        magic_preds,
        dummy,
    })
}

/// Why a demand program could not be made.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TransformError {
    #[error(
        "the rules are not stratified: a noValue reads facts that depend on its own rule, \
         so evaluating them on demand would depend on the order of evaluation"
    )]
    NotStratified,
    #[error("the demand program has {0} rules, more than the {MAX_RULES} allowed")]
    TooLarge(usize),
    #[error(transparent)]
    Memory(MemoryExceeded),
}
