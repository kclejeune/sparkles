//! Query-time reasoning (spec F17 Phase 1): derivation on demand against materialization,
//! backward rules, budgets, tables and EXPLAIN.

use oxrdf::{NamedNode, Term, Triple};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::{QueryOptions, QueryResult, explain, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::query::QueryReasoner;
use sparkles_reasoner::{
    Extras, INFERRED_GRAPH, Inputs, Profile, ReasonOptions, infer, materialize, tables,
};
use std::collections::BTreeSet;

const EX: &str = "http://ex.org/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

const PREFIXES: &str = "@prefix ex: <http://ex.org/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

/// xorshift64*
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn iri(s: String) -> NamedNode {
    NamedNode::new_unchecked(s)
}

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

fn prefixes() -> Vec<(String, String)> {
    vec![
        ("ex".into(), EX.into()),
        ("rdf".into(), RDF.into()),
        ("rdfs".into(), RDFS.into()),
        ("owl".into(), OWL.into()),
    ]
}

fn materialized() -> QueryOptions {
    QueryOptions {
        prefixes: prefixes(),
        default_graph_extra: vec![INFERRED_GRAPH.into()],
        ..Default::default()
    }
}

fn demand(r: &QueryReasoner) -> QueryOptions {
    QueryOptions {
        prefixes: prefixes(),
        reasoning: Some(r.provider()),
        ..Default::default()
    }
}

fn reasoner(p: &Profile) -> QueryReasoner {
    QueryReasoner::new(p.clone(), &Extras::default(), Inputs::default()).unwrap()
}

/// The solutions as sorted strings.
fn rows(r: &QueryResult) -> Vec<String> {
    let mut v: Vec<String> = r
        .rows()
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|t| t.map_or("UNDEF".into(), |t| t.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    v.sort();
    v
}

fn run(s: &Store, q: &str, o: &QueryOptions) -> QueryResult {
    query(s.snapshot(), q, o).unwrap_or_else(|e| panic!("{q}: {e}"))
}

/// A random triple over a small vocabulary (as in `incremental.rs`).
fn random_triple(r: &mut Rng, owl: bool) -> Triple {
    let c = |r: &mut Rng| iri(format!("{EX}C{}", r.below(6)));
    let p = |r: &mut Rng| iri(format!("{EX}p{}", r.below(5)));
    let i = |r: &mut Rng| iri(format!("{EX}i{}", r.below(10)));
    let v = |ns: &str, l: &str| iri(format!("{ns}{l}"));
    let k = r.below(if owl { 16 } else { 11 });
    let (s, pr, o): (NamedNode, NamedNode, Term) = match k {
        0 => (c(r), v(RDFS, "subClassOf"), c(r).into()),
        1 => (p(r), v(RDFS, "subPropertyOf"), p(r).into()),
        2 => (p(r), v(RDFS, "domain"), c(r).into()),
        3 => (p(r), v(RDFS, "range"), c(r).into()),
        4 | 5 => (i(r), v(RDF, "type"), c(r).into()),
        6..=8 => (i(r), p(r), i(r).into()),
        9 => (
            i(r),
            p(r),
            oxrdf::Literal::new_simple_literal(format!("v{}", r.below(3))).into(),
        ),
        10 => (i(r), p(r), oxrdf::Literal::from(r.below(4) as i64).into()),
        11 => (i(r), v(OWL, "sameAs"), i(r).into()),
        12 => (p(r), v(OWL, "inverseOf"), p(r).into()),
        13 => (
            p(r),
            v(RDF, "type"),
            v(
                OWL,
                [
                    "TransitiveProperty",
                    "SymmetricProperty",
                    "FunctionalProperty",
                ][r.below(3)],
            )
            .into(),
        ),
        14 => (c(r), v(OWL, "equivalentClass"), c(r).into()),
        _ => (c(r), v(OWL, "someValuesFrom"), c(r).into()),
    };
    Triple::new(s, pr, o)
}

/// A random term of a query position: a variable or a constant of the vocabulary.
fn random_term(r: &mut Rng, var: &str, kind: usize) -> String {
    if r.below(2) == 0 {
        return format!("?{var}");
    }
    match kind {
        0 => format!("ex:i{}", r.below(10)),
        1 => format!("ex:C{}", r.below(6)),
        _ => format!("ex:p{}", r.below(5)),
    }
}

fn random_pattern(r: &mut Rng, s: &str, o: &str) -> String {
    match r.below(4) {
        0 => format!("{} rdf:type {}", random_term(r, s, 0), random_term(r, o, 1)),
        1 => format!("{} ?p{s} {}", random_term(r, s, 0), random_term(r, o, 0)),
        _ => format!(
            "{} ex:p{} {}",
            random_term(r, s, 0),
            r.below(5),
            random_term(r, o, 0)
        ),
    }
}

fn rules_profile() -> Profile {
    Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [a: (?x ex:p0 ?y), (?y ex:p0 ?z), notEqual(?x, ?z) -> (?x ex:p1 ?z)]
         [b: (?x ex:p1 ?y) <- (?y ex:p1 ?x)]
         [c: (?x ex:p2 ?v), isLiteral(?v), strConcat(?v, '-x', ?w) -> (?x ex:p3 ?w)]
         [d: (?x ex:p4 ?v) <- (?x ex:p3 ?w), regex(?w, '(.*)-x', ?v)]
         [e: (?x ex:p4 ?v), lessThan(?v, 2) -> (?x rdf:type ex:C0)]
         [f: (?x rdf:type ex:C0), (?x ?p ?y), notEqual(?p, rdf:type) -> (?y rdf:type ex:C1)]
         [g: (?v ex:p2 ?x) -> (?x ex:back ?v)]
         [h: (?x rdf:type ex:C1), noValue(?x, ex:p0) -> (?x rdf:type ex:C2)]
         [anc1: (?x ex:anc ?y) <- (?x ex:p0 ?y)]
         [anc2: (?x ex:anc ?z) <- (?x ex:p0 ?y), (?y ex:anc ?z)]"
            .into(),
    )
}

/// The same rules as forward rules, which materialization evaluates.
fn forward(p: &Profile) -> Profile {
    match p {
        Profile::Rules(t) => {
            // `[name: head <- body]` as `[name: body -> head]`
            let re = regex::Regex::new(r"\[(\w+): (\([^\]]*?\)) <- ([^\]]*)\]").unwrap();
            Profile::Rules(re.replace_all(t, "[$1: $3 -> $2]").into_owned())
        }
        p => p.clone(),
    }
}

/// For random data and random queries of one or two patterns, the solutions under
/// `demand` equal those over a full materialization, for every profile.
#[test]
fn demand_answers_like_materialization() {
    let profiles = [
        (Profile::Rdfs, false),
        (Profile::RdfsSimple, false),
        (Profile::OwlRl, true),
        (rules_profile(), false),
    ];
    for (p, owl) in profiles {
        for seed in 1..=6u64 {
            let mut r = Rng(seed * 7919 + 13);
            let mut ttl = String::new();
            for _ in 0..50 {
                ttl.push_str(&format!("{} .\n", random_triple(&mut r, owl)));
            }
            let s = store(&ttl);
            materialize(&s, &forward(&p), &ReasonOptions::default())
                .unwrap_or_else(|e| panic!("{p}: {e:#}"));
            let qr = reasoner(&p);
            for _ in 0..12 {
                let one = random_pattern(&mut r, "a", "b");
                let two = random_pattern(&mut r, "b", "c");
                for q in [
                    format!("SELECT DISTINCT * {{ {one} }}"),
                    format!("SELECT DISTINCT * {{ {one} . {two} }}"),
                ] {
                    let want = rows(&run(&s, &q, &materialized()));
                    let got = rows(&run(&s, &q, &demand(&qr)));
                    assert_eq!(got, want, "{p} seed {seed}: {q}\n{ttl}");
                }
            }
        }
    }
}

/// A1: backward rules answer on demand and derive nothing outside the cone of the
/// query's constant, while materialization skips them.
#[test]
fn backward_rules_answer_on_demand() {
    let n = 2000;
    let mut ttl = String::new();
    for i in 0..n {
        ttl.push_str(&format!("ex:n{i} ex:parent ex:n{} .\n", i + 1));
    }
    let s = store(&ttl);
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [anc1: (?x ex:ancestor ?y) <- (?x ex:parent ?y)]
         [anc2: (?x ex:ancestor ?z) <- (?x ex:parent ?y), (?y ex:ancestor ?z)]"
            .into(),
    );
    let qr = reasoner(&rules);
    let r = run(&s, "SELECT ?a { ex:n10 ex:ancestor ?a }", &demand(&qr));
    assert_eq!(r.len(), n - 10);
    let applied = r.ctx.reasoning.clone().expect("reasoning applied");
    // linear in the chain: the answers about ex:n10 alone, so nothing about ex:n0 to
    // ex:n9 and not every pair of the chain
    assert_eq!(applied.report.derived, n as u64 - 10);
    let (derived, report) = infer(s.snapshot(), &rules, &ReasonOptions::default()).unwrap();
    assert!(derived.is_empty());
    assert!(
        report.warnings.iter().any(|w| w.contains("backward rules")),
        "{:?}",
        report.warnings
    );
}

/// A6: a derivation past the limit of derived facts fails with `reasoning-budget`,
/// naming the rule.
#[test]
fn budget_stops_a_runaway_rule() {
    let s = store("ex:a ex:val 0 .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [r: (?x ex:val ?w) <- (?x ex:val ?v), sum(?v, 1, ?w)]"
            .into(),
    );
    let qr = reasoner(&rules).with_max_derived(10_000);
    let Err(e) = query(s.snapshot(), "SELECT ?w { ex:a ex:val ?w }", &demand(&qr)) else {
        panic!("the derivation should pass its limit");
    };
    match &e {
        sparkles_core::Error::Component(c) => {
            assert_eq!(c.code, "reasoning-budget");
            assert!(c.message.contains("rule 'r'"), "{}", c.message);
            assert!(c.message.contains("10000"), "{}", c.message);
        }
        e => panic!("{e}"),
    }
}

/// A program whose `noValue` depends on its own rule is refused.
#[test]
fn unstratified_rules_are_refused() {
    let s = store("ex:a ex:p ex:b .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [r: (?x ex:q ?y) <- (?x ex:p ?y), noValue(?x, ex:q, ex:c)]
         [s: (?x ex:q ex:c) <- (?x ex:q ?y)]"
            .into(),
    );
    let qr = reasoner(&rules);
    let Err(e) = query(s.snapshot(), "SELECT ?y { ex:a ex:q ?y }", &demand(&qr)) else {
        panic!("the rules should be refused");
    };
    match &e {
        sparkles_core::Error::Component(c) => assert_eq!(c.code, "reasoning-rules"),
        e => panic!("{e}"),
    }
}

/// A pattern whose subject an earlier pattern binds is seeded with that pattern's
/// values: only the types of the course's students are derived.
#[test]
fn join_seeds_bound_variables() {
    let mut ttl = String::from(
        "ex:Grad rdfs:subClassOf ex:Student . ex:Student rdfs:subClassOf ex:Person .\n",
    );
    for i in 0..500 {
        ttl.push_str(&format!("ex:s{i} a ex:Grad ; ex:takes ex:c{} .\n", i % 50));
    }
    let s = store(&ttl);
    let qr = reasoner(&Profile::Rdfs);
    let q = "SELECT ?x { ?x ex:takes ex:c7 . ?x a ex:Person }";
    let r = run(&s, q, &demand(&qr));
    assert_eq!(r.len(), 10);
    let a = r.ctx.reasoning.clone().unwrap();
    // the ten students' types, and the schema the cone reads
    assert!(a.report.derived < 100, "derived {}", a.report.derived);
    let derive = a
        .nodes
        .iter()
        .find(|n| n.description.contains("Person"))
        .expect("a Derive node for the type pattern");
    assert_eq!(derive.operator, "Derive");
    let c = derive.counters.as_ref().unwrap();
    assert_eq!(c["seeds"], "join", "{c:?}");
    assert_eq!(c["seedCount"], 10, "{c:?}");
    // the plan shows the Reason node over the query's plan
    assert_eq!(r.plan.operator, "Reason");
}

/// Tables are cached per snapshot and view: the second run of a query reads them.
#[test]
fn tables_are_cached_per_snapshot() {
    let s = store("ex:C rdfs:subClassOf ex:B . ex:y a ex:C . ex:z a ex:B .");
    let qr = reasoner(&Profile::Rdfs);
    let q = "SELECT ?x { ?x a ex:B }";
    let first = run(&s, q, &demand(&qr));
    assert_eq!(rows(&first).len(), 2);
    let a = first.ctx.reasoning.clone().unwrap();
    assert_eq!((a.report.tables, a.report.cached), (1, 0));
    let second = run(&s, q, &demand(&qr));
    let b = second.ctx.reasoning.clone().unwrap();
    assert_eq!(b.report.cached, b.report.tables);
    assert_eq!(rows(&second), rows(&first));
    // a commit makes a new snapshot, whose tables are derived again
    s.load(&[Source::from_bytes(
        format!("{PREFIXES} ex:w a ex:C .").into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let third = run(&s, q, &demand(&qr));
    let c = third.ctx.reasoning.clone().unwrap();
    assert_eq!(c.report.cached, 0);
    assert_eq!(rows(&third).len(), 3);
    let _ = tables::stats();
}

/// A10: with RDFS on read set, demand reasoning applies instead and says so; it gives
/// full RDFS entailment.
#[test]
fn rdfs_on_read_does_not_apply_under_demand() {
    use sparkles_core::sparql::rdfs::{RdfsOnRead, RdfsSchema};
    let s = store(
        "ex:Student rdfs:subClassOf ex:Person . ex:attends rdfs:domain ex:Student .
         ex:tom ex:attends ex:course .",
    );
    let schema = RdfsSchema::from_triples(&[]);
    let qr = reasoner(&Profile::Rdfs);
    let opts = QueryOptions {
        rdfs: Some(std::sync::Arc::new(RdfsOnRead::fixed(schema))),
        ..demand(&qr)
    };
    let r = run(&s, "SELECT ?x { ?x a ex:Person }", &opts);
    assert_eq!(rows(&r), vec!["<http://ex.org/tom>"]);
    assert!(
        r.plan.description.contains("RDFS on read did not apply"),
        "{}",
        r.plan.description
    );
    let (_, plan) = explain(s.snapshot(), "SELECT ?x { ?x a ex:Person }", &opts).unwrap();
    assert_eq!(plan.operator, "Reason");
    assert_eq!(plan.children[0].operator, "Derive");
    assert!(
        plan.children[0].description.contains("rdf:type^fbb"),
        "{}",
        plan.children[0].description
    );
}

/// Derived facts with terms the store does not have reach the query, which can join on
/// them, and derived facts are not read from the inferred graph.
#[test]
fn derived_terms_and_no_inferred_graph() {
    let s = store("ex:a ex:name \"x\" .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [r: (?s ex:label ?l) <- (?s ex:name ?n), strConcat(?n, '!', ?l)]
         [t: (?s ex:shout ?l) <- (?s ex:label ?l)]"
            .into(),
    );
    materialize(&s, &rules, &ReasonOptions::default()).unwrap();
    let qr = reasoner(&rules);
    let r = run(
        &s,
        "SELECT ?l { ex:a ex:shout ?l . ex:a ex:label ?l }",
        &demand(&qr),
    );
    assert_eq!(rows(&r), vec!["\"x!\""]);
    // the inferred graph holds the forward closure of nothing: demand reads none of it
    let names: BTreeSet<String> = rows(&run(&s, "SELECT ?p { ex:a ?p ?o }", &demand(&qr)))
        .into_iter()
        .collect();
    assert!(names.contains("<http://ex.org/shout>"), "{names:?}");
}

/// A3 in small: with OWL 2 RL, a query for the instances of an intersection class that
/// a constant narrows asks only for the facts of the seeded students. The rules are
/// specialized to the schema, so the demand for `ex:Person` reads the facts of the
/// properties whose domain is a subclass of it for the seeds alone, and the intersection's
/// `listForAll` asks for the types of its members.
#[test]
fn owl_rl_demand_follows_the_schema() {
    let mut ttl = String::from(
        "ex:takes rdfs:domain ex:Student ; rdfs:range ex:Course .
         ex:teaches rdfs:domain ex:Teacher ; rdfs:range ex:Course .
         ex:memberOf rdfs:domain ex:Person .
         ex:Student rdfs:subClassOf ex:Person .
         ex:Teacher rdfs:subClassOf ex:Person .
         ex:GradCourse rdfs:subClassOf ex:Course .
         ex:GradStudent owl:intersectionOf ( ex:Person
             [ owl:onProperty ex:takes ; owl:someValuesFrom ex:GradCourse ] ) .\n",
    );
    for c in (0..30).step_by(2) {
        ttl.push_str(&format!("ex:c{c} a ex:GradCourse .\n"));
    }
    for i in 0..300 {
        ttl.push_str(&format!(
            "ex:s{i} ex:takes ex:c{} ; ex:memberOf ex:d{} .\n\
             ex:t{i} ex:teaches ex:c{} ; ex:memberOf ex:d{} .\n",
            i % 30,
            i % 3,
            i % 30,
            i % 3
        ));
    }
    let s = store(&ttl);
    let report = materialize(&s, &Profile::OwlRl, &ReasonOptions::default()).unwrap();
    let qr = reasoner(&Profile::OwlRl);
    let q = "SELECT ?x { ?x a ex:GradStudent . ?x ex:takes ex:c4 }";
    let want = rows(&run(&s, q, &materialized()));
    let r = run(&s, q, &demand(&qr));
    assert_eq!(rows(&r), want);
    assert_eq!(want.len(), 10);
    let a = r.ctx.reasoning.clone().unwrap();
    assert!(
        a.report.derived * 4 < report.inferred,
        "derived {} of the {} a materialization derives",
        a.report.derived,
        report.inferred
    );
}
