//! Closure operators in demand programs (spec F23 Phase 2): each kind of adorned copy of a
//! transitive rule engages an operator, and a session with operators derives exactly the
//! facts of one without them, which are the facts a materialization gives for its
//! demands.

use super::*;
use crate::ReasonOptions;
use crate::closure::{Counts, Stats};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::reasoning::DemandTerm;
use sparkles_core::store::{Store, StoreOptions};
use std::collections::BTreeSet;

const PREFIX: &str = "@prefix ex: <http://ex.org/> . @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix owl: <http://www.w3.org/2002/07/owl#> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> . ";

/// A store of `ttl` with facts enough that a session never takes the compact strategy,
/// which evaluates the source rules as joins.
fn store(ttl: &str) -> Store {
    let pad: String = (0..70)
        .map(|n| format!("ex:pad{n} ex:pad ex:pad{n} .\n"))
        .collect();
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            format!("{PREFIX}{ttl}\n{pad}").into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap_or_else(|e| panic!("{e:#}\n{ttl}"));
    store
}

fn session(store: &Store, profile: &Profile, on: bool) -> Session {
    let reasoner = QueryReasoner::new(profile.clone(), &Extras::default(), Inputs::default())
        .unwrap()
        .with_closures(on);
    let mut session = Session::new(
        reasoner.shared.clone(),
        SessionRequest {
            snap: store.snapshot(),
            readable: None,
            stop: Arc::new(|| Ok(())),
            max_memory: None,
            no_cache: true,
        },
    )
    .unwrap();
    // identical stores of two arms would share the schema's and equality's tables
    session.schema_view = None;
    session.make_graph().unwrap();
    session
}

fn term(x: &str) -> Term {
    if let Some(l) = x.strip_prefix('"') {
        let (lex, dt) = l.split_once("\"^^").expect("a typed literal");
        return Term::Literal(oxrdf::Literal::new_typed_literal(
            lex,
            oxrdf::NamedNode::new_unchecked(dt),
        ));
    }
    let iri = match x.split_once(':') {
        Some(("ex", l)) => format!("http://ex.org/{l}"),
        Some(("rdf", l)) => format!("http://www.w3.org/1999/02/22-rdf-syntax-ns#{l}"),
        Some(("owl", l)) => format!("http://www.w3.org/2002/07/owl#{l}"),
        _ => x.to_string(),
    };
    Term::NamedNode(oxrdf::NamedNode::new_unchecked(iri))
}

/// A demand with `?` for a free position.
fn demand(s: &str, p: &str, o: &str) -> Demand {
    let t = |x: &str| {
        if x == "?" {
            DemandTerm::Var
        } else {
            DemandTerm::Const(term(x))
        }
    };
    Demand {
        s: t(s),
        p: t(p),
        o: t(o),
    }
}

fn show(session: &Session, t: &[u64; 3]) -> String {
    let show = |x: u64| {
        session
            .terms
            .term(x)
            .map_or_else(|| format!("#{x}"), |t| t.to_string())
    };
    format!("{} {} {}", show(t[0]), show(t[1]), show(t[2]))
}

/// The live facts of a session's derived tier without its magic facts, written out,
/// those RDF cannot state included, with the facts it returned and how many it returned
/// in all.
fn returned(session: &Session) -> (BTreeSet<String>, BTreeSet<String>, usize) {
    let g = session.graph().unwrap();
    let live: BTreeSet<String> = g
        .facts()
        .enumerate()
        .filter(|(i, t)| g.alive(g.offset() + *i as u32) && !session.hidden.contains(&t[1]))
        .map(|(_, t)| show(session, &t))
        .collect();
    let mut n = 0;
    let mut out = BTreeSet::new();
    for c in &session.chunks {
        for t in c.iter() {
            n += 1;
            out.insert(show(session, t));
        }
    }
    (live, out, n)
}

/// The derived facts of a materialization that match `d`, written out.
fn materialized(ts: &[oxrdf::Triple], d: &Demand) -> BTreeSet<String> {
    let ok = |x: &DemandTerm, t: Term| match x {
        DemandTerm::Const(c) => *c == t,
        _ => true,
    };
    ts.iter()
        .filter(|t| {
            ok(&d.s, t.subject.clone().into())
                && ok(&d.p, t.predicate.clone().into())
                && ok(&d.o, t.object.clone())
        })
        .map(|t| format!("{} {} {}", t.subject, t.predicate, t.object))
        .collect()
}

/// The demands derived one call after the other in a session with operators and one
/// without, over a store each: both return the same facts, each once, and those of each
/// demand are the materialization's. Returns the operators' counts and the session.
fn compare(ttl: &str, profile: &Profile, demands: &[Demand]) -> (Stats, Session) {
    let calls: Vec<&[Demand]> = demands.iter().map(std::slice::from_ref).collect();
    compare_calls(ttl, profile, &calls)
}

/// [`compare`] with the demands of each call derived together.
fn compare_calls(ttl: &str, profile: &Profile, calls: &[&[Demand]]) -> (Stats, Session) {
    let demands: Vec<Demand> = calls.iter().flat_map(|c| c.iter().cloned()).collect();
    let (a, b) = (store(ttl), store(ttl));
    let mut with = session(&a, profile, true);
    let mut without = session(&b, profile, false);
    for c in calls {
        with.derive_all(c).unwrap();
        without.derive_all(c).unwrap();
    }
    assert!(!with.compact && !without.compact, "{ttl}");
    assert!(!with.cached_equality && !without.cached_equality);
    assert_eq!(without.closure_stats, Stats::default());
    let (w, wout, wn) = returned(&with);
    let (wo, woout, won) = returned(&without);
    assert_eq!(wn, wout.len(), "a fact returned twice\n{ttl}");
    assert_eq!(won, woout.len());
    assert_eq!(wout, woout, "{ttl}");
    if w != wo {
        let extra: Vec<_> = w.difference(&wo).take(10).collect();
        let missing: Vec<_> = wo.difference(&w).take(10).collect();
        panic!("{ttl}\n{demands:?}\nonly with operators: {extra:#?}\nonly without: {missing:#?}");
    }
    let (ts, _) = crate::infer(a.snapshot(), profile, &ReasonOptions::default()).unwrap();
    for d in &demands {
        let want = materialized(&ts, d);
        let got: BTreeSet<String> = {
            let ok = |x: &DemandTerm, v: &str| match x {
                DemandTerm::Const(c) => c.to_string() == v,
                _ => true,
            };
            w.iter()
                .filter(|f| {
                    let mut it = f.splitn(3, ' ');
                    let (s, p, o) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
                    ok(&d.s, s) && ok(&d.p, p) && ok(&d.o, o)
                })
                .cloned()
                .collect()
        };
        assert!(
            want.is_subset(&got),
            "{ttl}\n{d:?}: missing {:?}",
            want.difference(&got).collect::<Vec<_>>()
        );
    }
    (with.closure_stats, with)
}

fn rules(text: &str) -> Profile {
    Profile::Rules(format!("@prefix ex: <http://ex.org/> . {text}"))
}

fn chain(p: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("ex:a{i} {p} ex:a{} .\n", i + 1))
        .collect()
}

/// A transitive rule over a constant predicate: a demand that binds the subject walks
/// forward, one that binds the object walks backward, and one that binds neither closes
/// the relation.
#[test]
fn constant_transitive_copies_engage_by_shape() {
    let profile = rules("[t: (?a ex:p ?b), (?b ex:p ?c) -> (?a ex:p ?c)]");
    let ttl = chain("ex:p", 12) + "ex:b0 ex:p ex:a3 .\n";
    let (s, _) = compare(&ttl, &profile, &[demand("ex:a0", "ex:p", "?")]);
    assert!(s.forward.pairs > 0, "{s:?}");
    let (s, _) = compare(&ttl, &profile, &[demand("?", "ex:p", "ex:a12")]);
    assert!(s.backward.pairs > 0, "{s:?}");
    let (s, _) = compare(&ttl, &profile, &[demand("?", "ex:p", "?")]);
    assert!(s.full.pairs > 0, "{s:?}");
    // a filtered rule, read both ways
    let profile = rules("[t: (?a ex:p ?b), (?b ex:p ?c), notEqual(?a, ?c) -> (?a ex:p ?c)]");
    let ttl = chain("ex:p", 6) + "ex:a6 ex:p ex:a0 .\n";
    let (s, _) = compare(&ttl, &profile, &[demand("ex:a2", "ex:p", "?")]);
    assert!(s.forward.pairs > 0, "{s:?}");
    let (s, _) = compare(&ttl, &profile, &[demand("?", "ex:p", "ex:a2")]);
    assert!(s.backward.pairs > 0, "{s:?}");
}

/// `prp-trp` under each key: specialized to a constant predicate with its guard as a
/// constant atom (also through `prp-spo1`, which the schema specializes), and under keys
/// whose predicate a magic fact leaves free or holds (`Bound`, through `prp-symp`).
#[test]
fn prp_trp_copies_engage_under_each_key() {
    let profile = Profile::OwlRl;
    let ttl = chain("ex:p", 10)
        + "ex:p a owl:TransitiveProperty . ex:p rdfs:subPropertyOf ex:r . ex:q a owl:TransitiveProperty .\n"
        + &chain("ex:q", 4);
    let (s, _) = compare(&ttl, &profile, &[demand("ex:a0", "ex:p", "?")]);
    assert!(s.forward.pairs > 0, "{s:?}");
    let (s, _) = compare(&ttl, &profile, &[demand("ex:a0", "ex:r", "?")]);
    assert!(s.forward.pairs > 0, "{s:?}");
    let (s, _) = compare(&ttl, &profile, &[demand("?", "ex:r", "ex:a10")]);
    assert!(s.backward.pairs > 0, "{s:?}");
    let (s, _) = compare(&ttl, &profile, &[demand("ex:a0", "?", "?")]);
    assert!(s.forward.pairs > 0, "{s:?}");
    let (s, _) = compare(&ttl, &profile, &[demand("?", "?", "ex:a4")]);
    assert!(s.backward.pairs > 0, "{s:?}");
    // the guarded rule itself, which the schema does not specialize here
    let profile = rules(
        "[t: (?p ex:kind ex:Trans), (?x ?p ?y), (?y ?p ?z) -> (?x ?p ?z)]
         [s: (?p ex:kind ex:Sym), (?x ?p ?y) -> (?y ?p ?x)]
         [sp: (?p ex:sub ?q), (?x ?p ?y) -> (?x ?q ?y)]",
    );
    let ttl = chain("ex:h", 6)
        + &chain("ex:g", 5)
        + "ex:h ex:kind ex:Trans , ex:Sym . ex:g ex:kind ex:Trans . ex:g ex:sub ex:r .\n";
    let forward = |s: &Stats| s.forward.pairs > 0 && s.guarded.pairs > 0;
    let backward = |s: &Stats| s.backward.pairs > 0 && s.guarded.pairs > 0;
    for (d, engaged, key) in [
        (
            demand("ex:a2", "?", "?"),
            forward as fn(&Stats) -> bool,
            KeyP::Free,
        ),
        (demand("?", "?", "ex:a3"), backward, KeyP::Bound),
        (demand("ex:a1", "ex:r", "?"), forward, KeyP::Bound),
        (demand("?", "ex:r", "ex:a4"), backward, KeyP::Bound),
        (
            demand("?", "ex:h", "?"),
            |s: &Stats| s.full.pairs > 0,
            KeyP::Bound,
        ),
    ] {
        let (s, with) = compare(&ttl, &profile, std::slice::from_ref(&d));
        assert!(engaged(&s), "{d:?}: {s:?}");
        assert!(
            with.program.as_ref().unwrap().stages[0]
                .keys()
                .any(|(k, _)| k.p == key),
            "{d:?}: no {key:?} key"
        );
    }
}

/// The pairs an operator merges are in the closure of the relation, so they change what
/// no value reaches: another operator over the same predicate does not walk its values
/// again for them. Here a forward and a backward copy of `t` close one chain in one run,
/// and each walks only the values its magic facts add.
#[test]
fn merged_pairs_call_for_no_second_walk() {
    let profile = rules("[t: (?a ex:p ?b), (?b ex:p ?c) -> (?a ex:p ?c)]");
    let ttl = chain("ex:p", 10);
    let (s, _) = compare_calls(
        &ttl,
        &profile,
        &[&[demand("ex:a0", "ex:p", "?"), demand("?", "ex:p", "ex:a10")]],
    );
    assert!(s.forward.pairs > 0 && s.backward.pairs > 0, "{s:?}");
    assert!(s.walks > 0 && s.rewalks == 0, "{s:?}");
}

/// A copy that binds no end closes the whole relation, after which no operator over the
/// predicate walks it again until an edge that is not a merged pair arrives: the forward
/// copy of a call that also asks for every `ex:p` fact merges nothing for its new values,
/// nor walks for them.
#[test]
fn closed_relations_call_for_no_walk() {
    let profile = rules("[t: (?a ex:p ?b), (?b ex:p ?c) -> (?a ex:p ?c)]");
    let ttl = chain("ex:p", 10);
    let (s, _) = compare_calls(
        &ttl,
        &profile,
        &[&[demand("?", "ex:p", "?"), demand("ex:a0", "ex:p", "?")]],
    );
    assert!(s.full.pairs > 0, "{s:?}");
    assert!(s.walks > 0 && s.idle == 0, "{s:?}");
}

/// An edge that a join derives after an operator closed the relation calls for walks
/// again: `j` adds `ex:p` edges from `ex:q` facts that `k` derives an iteration later,
/// which extend the closed chain.
#[test]
fn join_edges_after_a_closure_walk_again() {
    let profile = rules(
        "[t: (?a ex:p ?b), (?b ex:p ?c) -> (?a ex:p ?c)]
         [k: (?x ex:r ?y) -> (?x ex:q ?y)]
         [j: (?x ex:q ?y) -> (?x ex:p ?y)]",
    );
    let ttl = chain("ex:p", 6) + "ex:a6 ex:r ex:b0 . ex:b0 ex:p ex:b1 . ex:b1 ex:r ex:b2 .\n";
    for calls in [
        &[demand("?", "ex:p", "?"), demand("ex:a0", "ex:p", "?")][..],
        &[demand("?", "ex:p", "?")][..],
        &[demand("ex:a2", "ex:p", "?")][..],
    ] {
        let (s, with) = compare_calls(&ttl, &profile, &[calls]);
        let (facts, _, _) = returned(&with);
        assert!(
            facts.contains("<http://ex.org/a2> <http://ex.org/p> <http://ex.org/b2>"),
            "{calls:?}: {s:?}\n{facts:#?}"
        );
    }
}

/// A transitive rule over a constant predicate answers a key that holds the predicate
/// as a value: a demand through the super-property of `ex:p` asks `sp` for `(?x ?p ?y)`
/// with `?p` bound at run time, so the copy of `t` under that `Bound` key has a magic atom
/// whose predicate slot is the constant `ex:p`, which each magic fact must match.
#[test]
fn constant_transitive_copies_answer_bound_predicate_keys() {
    let profile = rules(
        "[t: (?a ex:p ?b), (?b ex:p ?c) -> (?a ex:p ?c)]
         [sp: (?p ex:sub ?q), (?x ?p ?y) -> (?x ?q ?y)]",
    );
    // `ex:z` is asked for through `ex:s` alone, so a copy that took its magic facts for
    // those of `ex:p` would derive `ex:p` facts from it that the joins do not
    let ttl = chain("ex:p", 8)
        + "ex:p ex:sub ex:r . ex:q ex:sub ex:r .\n"
        + &chain("ex:q", 3)
        + "ex:z ex:p ex:a2 . ex:z ex:s ex:w . ex:s ex:sub ex:r2 .\n";
    for (d, engaged) in [
        (
            demand("ex:a0", "ex:r", "?"),
            (|s: &Stats| s.forward.pairs > 0) as fn(&Stats) -> bool,
        ),
        (demand("?", "ex:r", "ex:a8"), |s: &Stats| {
            s.backward.pairs > 0
        }),
    ] {
        let (s, with) = compare(&ttl, &profile, std::slice::from_ref(&d));
        assert!(engaged(&s) && s.bound_constant.pairs > 0, "{d:?}: {s:?}");
        assert_eq!(s.guarded, Counts::default(), "{d:?}: {s:?}");
        assert!(
            with.program.as_ref().unwrap().stages[0]
                .keys()
                .any(|(k, _)| k.p == KeyP::Bound),
            "{d:?}: no Bound key"
        );
    }
    compare_calls(
        &ttl,
        &profile,
        &[&[demand("ex:a0", "ex:r", "?"), demand("ex:z", "ex:r2", "?")]],
    );
}

/// The equality stage derives `owl:sameAs` with the free adornment, where `eq-trans`
/// closes the relation.
#[test]
fn eq_trans_engages_in_the_equality_stage() {
    let ttl: String = (0..8)
        .map(|i| format!("ex:s{i} owl:sameAs ex:s{} .\n", i + 1))
        .collect::<String>()
        + "ex:s0 a ex:C . ex:s8 ex:knows ex:t .\n";
    let (s, _) = compare(
        &ttl,
        &Profile::OwlRl,
        &[demand("?", "rdf:type", "ex:C"), demand("ex:s3", "?", "?")],
    );
    assert!(s.full.pairs > 0, "{s:?}");
}

/// A session derives demands one after the other, so that later runs start from facts
/// closed for the earlier ones: a bound key first and the free key later, and new values
/// asked for under a key the session already holds.
#[test]
fn later_demands_close_from_the_marks() {
    let profile = rules("[t: (?a ex:p ?b), (?b ex:p ?c) -> (?a ex:p ?c)]");
    let ttl = chain("ex:p", 10) + "ex:z ex:p ex:a5 . ex:a10 ex:p ex:y .\n";
    let (s, _) = compare(
        &ttl,
        &profile,
        &[
            demand("ex:a7", "ex:p", "?"),
            demand("ex:a2", "ex:p", "?"),
            demand("?", "ex:p", "ex:a4"),
            demand("?", "ex:p", "?"),
            demand("ex:z", "ex:p", "?"),
        ],
    );
    assert!(
        s.forward.steps > 1 && s.backward.pairs > 0 && s.full.pairs > 0,
        "{s:?}"
    );
    let profile = Profile::OwlRl;
    let ttl = chain("ex:p", 8)
        + "ex:p a owl:TransitiveProperty . ex:p rdfs:subPropertyOf ex:r . ex:p a owl:SymmetricProperty .\n";
    compare(
        &ttl,
        &profile,
        &[
            demand("ex:a6", "ex:r", "?"),
            demand("ex:a1", "?", "?"),
            demand("?", "ex:r", "ex:a3"),
            demand("?", "ex:p", "?"),
        ],
    );
}

/// An operator gives a filtered rule back when the relation has a literal subject, which
/// for a copy read backward is the end of a reversed edge: the rule's `notEqual` compares
/// the values of `"1"` and `"01"`, which are equal.
#[test]
fn reversed_walks_give_filtered_rules_back_at_literals() {
    let profile = rules(
        "[t: (?a ex:p ?b), (?b ex:p ?c), notEqual(?a, ?c) -> (?a ex:p ?c)]
         [i: (?x ex:val ?l) -> (?l ex:p ?x)]",
    );
    let ttl = "ex:a ex:p \"01\"^^xsd:integer . ex:a ex:val \"1\"^^xsd:integer . ex:b ex:p ex:a .";
    let one = "\"01\"^^http://www.w3.org/2001/XMLSchema#integer";
    let (_, with) = compare(ttl, &profile, &[demand("?", "ex:p", one)]);
    let (facts, _, _) = returned(&with);
    // the literal subject is derived, and the pair of equal values is not
    assert!(facts.iter().any(|f| f.starts_with("\"1\"")), "{facts:#?}");
    let zero_one = "\"01\"^^<http://www.w3.org/2001/XMLSchema#integer>";
    assert!(
        !facts
            .iter()
            .any(|f| f.starts_with("\"1\"") && f.ends_with(zero_one)),
        "{facts:#?}"
    );
}

/// A guarded rule whose guard admits `rdf:rest` gives its copies back to the joins, which
/// close it, since the operators of a demand program never close list predicates.
#[test]
fn guarded_copies_give_list_predicates_back() {
    let profile = rules("[t: (?p ex:kind ex:Trans), (?x ?p ?y), (?y ?p ?z) -> (?x ?p ?z)]");
    let ttl = "rdf:rest ex:kind ex:Trans . ex:a rdf:rest ex:b . ex:b rdf:rest ex:c .
        ex:h ex:kind ex:Trans . ex:a ex:h ex:b . ex:b ex:h ex:c .";
    let (_, with) = compare(ttl, &profile, &[demand("ex:a", "?", "?")]);
    let (facts, _, _) = returned(&with);
    let rest =
        "<http://ex.org/a> <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> <http://ex.org/c>";
    assert!(facts.contains(rest), "{facts:#?}");
}

/// A run that derives a list cell that its rules read after an operator emitted starts
/// again without operators: its rows are killed, the session's later runs use joins, and
/// no fact goes out twice.
#[test]
fn derived_list_cells_restart_the_run_without_operators() {
    let ttl = "ex:a owl:sameAs ex:b . ex:b owl:sameAs ex:c . ex:c owl:sameAs ex:l .
        ex:l rdf:first ex:x ; rdf:rest rdf:nil .
        ex:K owl:intersectionOf ( ex:C ex:D ) . ex:x a ex:C , ex:D .";
    let (_, with) = compare(
        ttl,
        &Profile::OwlRl,
        &[demand("?", "rdf:type", "ex:K"), demand("ex:a", "?", "?")],
    );
    assert!(with.closures_off);
}

/// xorshift
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Random property graphs with transitive, symmetric, inverse, functional and inverse
/// functional properties, property hierarchies and chains of two and three properties,
/// sameAs between individuals and with literals, literal values whose ids differ for
/// equal values, and classes, and random demands of every shape, derived one after the
/// other. `SPARKLES_CLOSURE_CASES` sets the number of cases (6 by default).
#[test]
fn random_sessions_derive_what_the_joins_derive() {
    let mut engaged = Stats::default();
    let cases: u64 = std::env::var("SPARKLES_CLOSURE_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    for seed in 0..cases {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let (props, inds) = (2 + r.below(3), 5 + r.below(10));
        let mut t = String::new();
        for i in 0..props {
            if r.below(100) < 60 {
                t += &format!("ex:p{i} a owl:TransitiveProperty .\n");
            }
            if r.below(100) < 30 {
                t += &format!("ex:p{i} a owl:SymmetricProperty .\n");
            }
            if r.below(100) < 20 {
                t += &format!("ex:p{i} owl:inverseOf ex:p{} .\n", r.below(props));
            }
            if r.below(100) < 30 {
                t += &format!("ex:p{i} rdfs:subPropertyOf ex:p{} .\n", r.below(props));
            }
            if r.below(100) < 20 {
                t += &format!("ex:p{i} rdfs:domain ex:C{} .\n", r.below(3));
            }
            if r.below(100) < 15 {
                t += &format!("ex:p{i} a owl:FunctionalProperty .\n");
            }
            if r.below(100) < 10 {
                t += &format!("ex:p{i} a owl:InverseFunctionalProperty .\n");
            }
            match r.below(100) {
                0..15 => {
                    t += &format!(
                        "ex:p{i} owl:propertyChainAxiom ( ex:p{} ex:p{} ) .\n",
                        r.below(props),
                        r.below(props)
                    );
                }
                15..20 => {
                    t += &format!(
                        "ex:p{i} owl:propertyChainAxiom ( ex:p{} ex:p{} ex:p{} ) .\n",
                        r.below(props),
                        r.below(props),
                        r.below(props)
                    );
                }
                _ => {}
            }
        }
        // values whose ids differ although `1` and `01` are equal
        let lits = [
            "\"1\"^^xsd:integer",
            "\"01\"^^xsd:integer",
            "\"2\"^^xsd:integer",
        ];
        if r.below(100) < 40 {
            for _ in 0..1 + r.below(3) {
                t += &format!(
                    "ex:i{} ex:p{} {} .\n",
                    r.below(inds),
                    r.below(props),
                    lits[r.below(3) as usize]
                );
            }
        }
        if r.below(100) < 25 {
            t += &format!(
                "ex:i{} owl:sameAs {} .\n",
                r.below(inds),
                lits[r.below(3) as usize]
            );
        }
        for _ in 0..2 * inds {
            t += &format!(
                "ex:i{} ex:p{} ex:i{} .\n",
                r.below(inds),
                r.below(props),
                r.below(inds)
            );
        }
        for _ in 0..r.below(3) {
            t += &format!("ex:i{} owl:sameAs ex:i{} .\n", r.below(inds), r.below(inds));
        }
        let mut demands = Vec::new();
        for _ in 0..3 {
            let i = format!("ex:i{}", r.below(inds));
            let p = format!("ex:p{}", r.below(props));
            let lit = ["1", "01", "2"][r.below(3) as usize];
            let lit = format!("\"{lit}\"^^http://www.w3.org/2001/XMLSchema#integer");
            demands.push(match r.below(8) {
                0 => demand(&i, &p, "?"),
                1 => demand("?", &p, &i),
                2 => demand(&i, "?", "?"),
                3 => demand("?", "?", &i),
                4 => demand("?", &p, "?"),
                5 => demand("?", "owl:sameAs", &i),
                6 => demand("?", &p, &lit),
                _ => demand("?", "rdf:type", &format!("ex:C{}", r.below(3))),
            });
        }
        let (s, _) = compare(&format!("# seed {seed}\n{t}"), &Profile::OwlRl, &demands);
        engaged.add(&s);
    }
    assert!(
        engaged.forward.pairs + engaged.backward.pairs + engaged.full.pairs > 0,
        "{engaged:?}"
    );
}
