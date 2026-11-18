//! Rule programs through the public paths (item 839, Phase 5): a rule text with
//! `.decl`, relation atoms, `not` and `head :- body.` clauses (spec C24 §6.2) is
//! materialized, maintained, explained, given witnesses and answered at query time.

use oxrdf::{GraphName, NamedNode, Quad, Term, Triple};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, QueryResult, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::explain::{Derivation, ExplainInput, ExplainOptions, Explained, explain};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{
    Cache, Extras, INFERRED_GRAPH, Incremental, Inputs, Method, Profile, ReasonOptions,
    ReasonReport, materialize_incremental, parse_rules, rules_text,
};
use std::collections::BTreeSet;

const EX: &str = "http://ex.org/";
const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
";

/// Property chains and keys of any length as rule text, after `owl2rl.rls`, with the
/// chain's subject first: `chain(x, h, r, y)` says that the properties of the chain axiom
/// of `h` before its list cell `r` lead from `x` to `y`, and `keyed(x, y, c, r)` that `x`
/// and `y` of class `c` share the values of the key's properties before `r`.
const CHAINS_AND_KEYS: &str = "@prefix ex: <http://ex.org/> .
.decl chain(x, h, rest, y)
.decl keyed(x, y, c, rest)
chain(?x, ?h, ?r, ?y) :- (?h owl:propertyChainAxiom ?l), (?l rdf:first ?p), (?l rdf:rest ?r), (?x ?p ?y).
chain(?x, ?h, ?r2, ?z) :- chain(?x, ?h, ?r, ?y), (?r rdf:first ?p), (?r rdf:rest ?r2), (?y ?p ?z).
[chain-end: chain(?x, ?h, rdf:nil, ?y) -> (?x ?h ?y)]
keyed(?x, ?y, ?c, ?r) :- (?c owl:hasKey ?l), (?l rdf:first ?p), (?l rdf:rest ?r), (?x rdf:type ?c), (?y rdf:type ?c), (?x ?p ?v), (?y ?p ?v), notEqual(?x, ?y).
keyed(?x, ?y, ?c, ?r2) :- keyed(?x, ?y, ?c, ?r), (?r rdf:first ?p), (?r rdf:rest ?r2), (?x ?p ?v), (?y ?p ?v).
(?x owl:sameAs ?y) :- keyed(?x, ?y, ?c, rdf:nil).
";

fn store(ttl: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!("{PREFIXES}{ttl}").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn iri(l: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{l}"))
}

fn ex(l: &str) -> Term {
    Term::NamedNode(iri(l))
}

fn change(s: &Store, add: &[Triple], del: &[Triple]) {
    let quad = |t: &Triple| {
        Quad::new(
            t.subject.clone(),
            t.predicate.clone(),
            t.object.clone(),
            GraphName::DefaultGraph,
        )
    };
    let mut txn = s.write();
    let mut labels = Default::default();
    for t in del {
        let q = txn.encode_quad(&quad(t), &mut labels).unwrap();
        txn.delete(q).unwrap();
    }
    for t in add {
        let q = txn.encode_quad(&quad(t), &mut labels).unwrap();
        txn.insert(q).unwrap();
    }
    txn.commit().unwrap();
}

fn t(s: &str, p: &str, o: &str) -> Triple {
    Triple::new(iri(s), iri(p), ex(o))
}

fn rules(text: &str) -> Profile {
    Profile::Rules(text.to_string())
}

fn reason(s: &Store, p: &Profile, since: Option<u64>, cache: Option<&Cache>) -> ReasonReport {
    reason_with(s, p, since, cache, &ReasonOptions::default())
}

fn reason_with(
    s: &Store,
    p: &Profile,
    since: Option<u64>,
    cache: Option<&Cache>,
    opts: &ReasonOptions,
) -> ReasonReport {
    materialize_incremental(s, p, &Extras::default(), Incremental { since, cache }, opts)
        .unwrap_or_else(|e| panic!("{e:#}"))
}

fn commit(r: &ReasonReport) -> u64 {
    r.receipt.as_ref().map(|r| r.commit.seq).unwrap_or(0)
}

fn rows(r: &QueryResult) -> BTreeSet<String> {
    r.rows()
        .unwrap()
        .iter()
        .map(|row| {
            row.values()
                .iter()
                .map(|t| t.as_ref().map_or("UNDEF".into(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The triples of the inferred graph.
fn inferred(s: &Store) -> BTreeSet<String> {
    let q = format!("SELECT ?s ?p ?o WHERE {{ GRAPH <{INFERRED_GRAPH}> {{ ?s ?p ?o }} }}");
    rows(&query(s.snapshot(), &q, &QueryOptions::default()).unwrap())
}

/// A chain of `n` properties `ex:c{n}_1 … ex:c{n}_n` for `ex:h{n}`, walked by
/// `ex:a{n}_0 … ex:a{n}_n`, with a branch at the second step.
fn chain_data(n: usize) -> String {
    let props: Vec<String> = (1..=n).map(|i| format!("ex:c{n}_{i}")).collect();
    let mut ttl = format!("ex:h{n} owl:propertyChainAxiom ( {} ) .\n", props.join(" "));
    for i in 1..=n {
        ttl.push_str(&format!("ex:a{n}_{} ex:c{n}_{i} ex:a{n}_{i} .\n", i - 1));
    }
    ttl.push_str(&format!("ex:a{n}_1 ex:c{n}_2 ex:b{n} .\n"));
    ttl
}

/// What the chain of `chain_data(n)` gives: `a_0 h a_n`, and `a_0 h b` when `n` is 2.
fn chain_facts(n: usize) -> BTreeSet<String> {
    let mut want = BTreeSet::from([format!("<{EX}a{n}_0> <{EX}h{n}> <{EX}a{n}_{n}>")]);
    if n == 2 {
        want.insert(format!("<{EX}a2_0> <{EX}h2> <{EX}b2>"));
    }
    want
}

fn with_pred(all: &BTreeSet<String>, p: &str) -> BTreeSet<String> {
    all.iter()
        .filter(|r| r.split(' ').nth(1) == Some(p))
        .cloned()
        .collect()
}

/// Chains of 2, 3, 4, 5 and 8 properties: one program covers every length, and at
/// lengths 2 and 3 it agrees with `owl-rl`'s rules per length.
#[test]
fn chains_of_any_length() {
    let lengths = [2, 3, 4, 5, 8];
    let data: String = lengths.iter().map(|&n| chain_data(n)).collect();
    let s = store(&data);
    let r = reason(&s, &rules(CHAINS_AND_KEYS), None, None);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let got = inferred(&s);
    for n in lengths {
        let h = format!("<{EX}h{n}>");
        assert_eq!(with_pred(&got, &h), chain_facts(n), "length {n}");
    }
    // no relation fact reaches the dataset
    assert!(
        got.iter().all(|r| !r.contains("urn:x-sparkles:rel:")),
        "{got:?}"
    );
    let owl = store(&data);
    reason(&owl, &Profile::OwlRl, None, None);
    let rl = inferred(&owl);
    for n in [2, 3] {
        let h = format!("<{EX}h{n}>");
        assert_eq!(with_pred(&rl, &h), chain_facts(n), "owl-rl at length {n}");
    }
}

/// A key of 5 properties with literal and IRI values derives `owl:sameAs` only for the
/// individuals that share every value.
#[test]
fn keys_of_any_length() {
    let s = store(
        "ex:K owl:hasKey ( ex:k1 ex:k2 ex:k3 ex:k4 ex:k5 ) .
         ex:x a ex:K ; ex:k1 1 ; ex:k2 'two' ; ex:k3 ex:v3 ; ex:k4 4 ; ex:k5 ex:v5 .
         ex:y a ex:K ; ex:k1 1 ; ex:k2 'two' ; ex:k3 ex:v3 ; ex:k4 4 ; ex:k5 ex:v5 .
         ex:z a ex:K ; ex:k1 1 ; ex:k2 'two' ; ex:k3 ex:v3 ; ex:k4 4 ; ex:k5 ex:other .",
    );
    reason(&s, &rules(CHAINS_AND_KEYS), None, None);
    let same = with_pred(&inferred(&s), "<http://www.w3.org/2002/07/owl#sameAs>");
    let want = BTreeSet::from([
        format!("<{EX}x> <http://www.w3.org/2002/07/owl#sameAs> <{EX}y>"),
        format!("<{EX}y> <http://www.w3.org/2002/07/owl#sameAs> <{EX}x>"),
    ]);
    assert_eq!(same, want);
}

/// Spec C24 §6.2's ancestors, with stratified negation: `unrelated` holds for the
/// people of the data that are not ancestors of `ex:n0`'s descendants' line.
#[test]
fn ancestors_with_negation() {
    let text = "@prefix ex: <http://ex.org/> .
.decl ancestor(a, d)
.decl person(x)
ancestor(?a, ?d) :- (?a ex:parent ?d).
ancestor(?a, ?d) :- (?a ex:parent ?m), ancestor(?m, ?d).
person(?x) :- (?x rdf:type ex:Person).
(?x ex:unrelatedTo ex:n0) :- person(?x), not ancestor(?x, ex:n0), not ancestor(ex:n0, ?x), notEqual(?x, ex:n0).
(?a ex:ancestorOf ?d) :- ancestor(?a, ?d).
";
    let s = store(
        "ex:n2 ex:parent ex:n1 . ex:n1 ex:parent ex:n0 . ex:n0 ex:parent ex:m .
         ex:n0 a ex:Person . ex:n1 a ex:Person . ex:n2 a ex:Person . ex:m a ex:Person .
         ex:q a ex:Person .",
    );
    let r = reason(&s, &rules(text), None, None);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let got = inferred(&s);
    let unrelated = with_pred(&got, &format!("<{EX}unrelatedTo>"));
    assert_eq!(
        unrelated,
        BTreeSet::from([format!("<{EX}q> <{EX}unrelatedTo> <{EX}n0>")])
    );
    assert_eq!(with_pred(&got, &format!("<{EX}ancestorOf>")).len(), 6);
}

/// Kept and cache-free updates of a chain program equal full runs: one keeps its closure
/// in memory, and one reads it back from a persistent dataset, which holds no relation
/// fact, so the update rebuilds the relation first. A program that negates runs in full
/// on an update.
#[test]
fn updates_equal_full_runs() {
    for kept in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let s = if kept {
            store(&chain_data(4))
        } else {
            let s = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
            s.load(&[Source::from_bytes(
                format!("{PREFIXES}{}", chain_data(4)).into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
            s
        };
        let cache = Cache::default();
        let cache = kept.then_some(&cache);
        let p = rules(CHAINS_AND_KEYS);
        let r = reason(&s, &p, None, cache);
        let since = commit(&r);
        // a second path through the chain, and the first path broken
        change(
            &s,
            &[t("a4_0", "c4_1", "d1"), t("d1", "c4_2", "a4_2")],
            &[t("a4_1", "c4_2", "a4_2")],
        );
        let r = reason(&s, &p, Some(since), cache);
        assert_eq!(
            r.method,
            Method::Incremental,
            "kept {kept}: {:?}",
            r.fallback
        );
        let source = r.changes.as_ref().map(|c| c.source.clone());
        assert_eq!(
            source.as_deref(),
            Some(if kept { "memory" } else { "store" })
        );
        let got = inferred(&s);
        // the same data, materialized in full
        let mut ttl = chain_data(4).replace("ex:a4_1 ex:c4_2 ex:a4_2 .\n", "");
        ttl.push_str("ex:a4_0 ex:c4_1 ex:d1 . ex:d1 ex:c4_2 ex:a4_2 .\n");
        let fresh = store(&ttl);
        reason(&fresh, &p, None, None);
        assert_eq!(got, inferred(&fresh), "kept {kept}");
        assert!(got.contains(&format!("<{EX}a4_0> <{EX}h4> <{EX}a4_4>")));
    }
    let negating = "@prefix ex: <http://ex.org/> .
.decl r(x)
r(?x) :- (?x ex:p ?y).
(?x ex:q ex:none) :- (?x rdf:type ex:T), not r(?x).
";
    let s = store("ex:a a ex:T . ex:b a ex:T . ex:a ex:p ex:c .");
    let r = reason(&s, &rules(negating), None, None);
    let since = commit(&r);
    assert_eq!(
        with_pred(&inferred(&s), &format!("<{EX}q>")),
        BTreeSet::from([format!("<{EX}b> <{EX}q> <{EX}none>")])
    );
    change(&s, &[t("b", "p", "c")], &[]);
    let r = reason(&s, &rules(negating), Some(since), None);
    assert_eq!(r.method, Method::Full);
    assert!(
        r.fallback.as_deref().is_some_and(|f| f.contains("negates")),
        "{:?}",
        r.fallback
    );
    assert!(with_pred(&inferred(&s), &format!("<{EX}q>")).is_empty());
}

fn why(s: &Store, p: &Profile, t: [Term; 3]) -> Explained {
    let snap = s.snapshot();
    let inferred = snap.lookup_iri(INFERRED_GRAPH);
    explain(
        &ExplainInput {
            snap,
            profile: p,
            extras: &Extras::default(),
            inputs: &[sparkles_core::id::Id::DEFAULT_GRAPH],
            inferred,
        },
        &t,
        &ExplainOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{e:#}"))
}

fn tree(e: Explained) -> Derivation {
    match e {
        Explained::Found { mut trees, .. } => trees.remove(0),
        other => panic!("no tree: {other:?}"),
    }
}

fn relation_names(d: &Derivation, out: &mut BTreeSet<String>) {
    if let Some(r) = &d.relation {
        out.insert(r.name.clone());
    }
    for p in &d.premises {
        relation_names(p, out);
    }
}

/// An explanation through a chain shows relation nodes, with the rule names, lines and
/// variable names of the rule text.
#[test]
fn explanations_pass_relations() {
    let s = store(&chain_data(4));
    let p = rules(CHAINS_AND_KEYS);
    reason(&s, &p, None, None);
    let d = tree(why(&s, &p, [ex("a4_0"), ex("h4"), ex("a4_4")]));
    assert_eq!(d.rule.as_ref().unwrap().name, "chain-end");
    let mut names = BTreeSet::new();
    relation_names(&d, &mut names);
    assert_eq!(names, BTreeSet::from(["chain".to_string()]));
    let bound: BTreeSet<&str> = d.bindings.iter().map(|(n, _)| n.as_str()).collect();
    assert!(bound.contains("x") && bound.contains("y"), "{bound:?}");
}

/// A Jena rule inside a rule program keeps its variable names in explanations, and a
/// relation rule's constants that the store does not hold are compiled over the
/// explanation's own terms.
#[test]
fn explanations_of_mixed_programs() {
    let text = "@prefix ex: <http://ex.org/> .
.decl tag(x, t)
tag(?x, ?t) :- (?x ex:p ?y), strConcat('a', 'b', ?t).
[copy: (?x ex:q ?who) -> (?x ex:r ?who)]
(?x ex:tagged ?t) :- tag(?x, ?t).
";
    let s = store("ex:i ex:p ex:j . ex:i ex:q ex:k .");
    let p = rules(text);
    reason(&s, &p, None, None);
    let d = tree(why(&s, &p, [ex("i"), ex("r"), ex("k")]));
    let bound: BTreeSet<&str> = d.bindings.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(bound, BTreeSet::from(["x", "who"]));
    let lit = Term::Literal(oxrdf::Literal::new_simple_literal("ab"));
    let d = tree(why(&s, &p, [ex("i"), ex("tagged"), lit]));
    let mut names = BTreeSet::new();
    relation_names(&d, &mut names);
    assert_eq!(names, BTreeSet::from(["tag".to_string()]));
}

/// Witnesses of a fact derived through a relation are sets of input facts.
#[test]
fn witnesses_pass_relations() {
    let s = store(&chain_data(2));
    let cache = Cache::default();
    let opts = ReasonOptions {
        witnesses: Some(8),
        ..Default::default()
    };
    let r = reason_with(&s, &rules(CHAINS_AND_KEYS), None, Some(&cache), &opts);
    let w = r.witnesses.expect("witnesses");
    assert!(w.built && w.truncated == 0, "{w:?}");
    let got = cache
        .witnesses(&Triple::new(iri("a2_0"), iri("h2"), ex("b2")))
        .expect("kept");
    assert_eq!(got.annotation.witnesses.len(), 1);
}

fn demand(r: &QueryReasoner) -> QueryOptions {
    QueryOptions {
        prefixes: vec![("ex".into(), EX.into())],
        reasoning: Some(r.provider()),
        ..Default::default()
    }
}

/// Query-time answers over a rule program equal the materialization, for a bound
/// subject, a bound object and a free pattern.
#[test]
fn query_time_answers_equal_materialization() {
    let data: String = [2, 4, 5].iter().map(|&n| chain_data(n)).collect();
    let p = rules(CHAINS_AND_KEYS);
    let s = store(&data);
    let r = QueryReasoner::new(p.clone(), &Extras::default(), Inputs::default()).unwrap();
    let m = store(&data);
    reason(&m, &p, None, None);
    let mat = QueryOptions {
        prefixes: vec![("ex".into(), EX.into())],
        default_graph_extra: vec![INFERRED_GRAPH.into()],
        ..Default::default()
    };
    for q in [
        "SELECT ?y WHERE { ex:a4_0 ex:h4 ?y }",
        "SELECT ?x WHERE { ?x ex:h5 ex:a5_5 }",
        "SELECT ?x ?y WHERE { ?x ex:h2 ?y }",
        "SELECT ?x ?p ?y WHERE { ?x ?p ?y }",
    ] {
        let got = rows(&query(s.snapshot(), q, &demand(&r)).unwrap());
        let want = rows(&query(m.snapshot(), q, &mat).unwrap());
        assert_eq!(got, want, "{q}");
        assert!(!got.is_empty(), "{q}");
    }
}

/// The parsed rules of a program read back from [`rules_text`].
#[test]
fn rule_text_round_trips() {
    let parsed = parse_rules(CHAINS_AND_KEYS).unwrap();
    let again = parse_rules(&rules_text(&parsed)).unwrap();
    let strip = |rs: &[sparkles_reasoner::Rule]| -> Vec<String> {
        rs.iter().map(ToString::to_string).collect()
    };
    assert_eq!(strip(&parsed), strip(&again));
}

/// A program that does not stratify, and one with an existential head, are refused with
/// their reasons.
#[test]
fn refused_programs() {
    let s = store("ex:a ex:p ex:b .");
    let cyclic = "@prefix ex: <http://ex.org/> .
.decl r(x)
r(?x) :- (?x ex:p ?y), not r(?y).
";
    let e = materialize_incremental(
        &s,
        &rules(cyclic),
        &Extras::default(),
        Incremental::default(),
        &ReasonOptions::default(),
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("r"), "{e:#}");
    let existential = "@prefix ex: <http://ex.org/> .
.decl advisor(s, a)
[adv: (?s rdf:type ex:Student) -> advisor(?s, ?a)]
(?s ex:hasAdvisor ?a) :- advisor(?s, ?a).
";
    let e = materialize_incremental(
        &s,
        &rules(existential),
        &Extras::default(),
        Incremental::default(),
        &ReasonOptions::default(),
    )
    .unwrap_err();
    assert!(format!("{e:#}").contains("adv"), "{e:#}");
}
