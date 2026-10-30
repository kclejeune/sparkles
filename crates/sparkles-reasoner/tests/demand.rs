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

/// Codex review F17-P1 #1: imports are followed only through graphs the caller reads, so
/// a denied graph neither adds its schema nor names the graphs it imports.
#[test]
fn imports_follow_the_graph_view() {
    use sparkles_core::access::{GraphAccess, GraphRule, Graphs};
    use std::sync::Arc;
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        format!(
            "{PREFIXES}
             <urn:o> owl:imports <http://ex.org/hidden> . ex:tom a ex:Cat .
             GRAPH <http://ex.org/hidden> {{
                 <urn:h> owl:imports <http://secret/graph> .
                 ex:Cat rdfs:subClassOf ex:Animal .
             }}"
        )
        .into_bytes(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let qr = reasoner(&Profile::Rdfs);
    let q = "SELECT ?x { ?x a ex:Animal }";
    // with every graph readable the import applies
    assert_eq!(rows(&run(&s, q, &demand(&qr))), vec!["<http://ex.org/tom>"]);
    let mut o = demand(&qr);
    o.graphs = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["default"], &[])),
        Graphs::none(),
    )));
    let r = run(&s, q, &o);
    assert!(rows(&r).is_empty(), "{:?}", rows(&r));
    let warnings: Vec<String> = r.ctx.warnings().into_iter().map(|w| w.message).collect();
    assert!(
        !warnings.iter().any(|w| w.contains("secret")),
        "{warnings:?}"
    );
}

/// Codex review F17-P1 #2: the linear rewriting keeps the stored facts of the recursive
/// predicate at the nodes it reaches, from either end.
#[test]
fn linear_recursion_reads_stored_facts() {
    let s = store("ex:a ex:parent ex:b . ex:b ex:ancestor ex:c .");
    let both = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [anc1: (?x ex:ancestor ?y) <- (?x ex:parent ?y)]
         [anc2: (?x ex:ancestor ?z) <- (?x ex:parent ?y), (?y ex:ancestor ?z)]"
            .into(),
    );
    let rec = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [anc2: (?x ex:ancestor ?z) <- (?x ex:parent ?y), (?y ex:ancestor ?z)]"
            .into(),
    );
    let qb = reasoner(&both);
    let qr = reasoner(&rec);
    let down = "SELECT ?z { ex:a ex:ancestor ?z }";
    let up = "SELECT ?x { ?x ex:ancestor ex:c }";
    assert_eq!(
        rows(&run(&s, down, &demand(&qb))),
        vec!["<http://ex.org/b>", "<http://ex.org/c>"]
    );
    assert_eq!(
        rows(&run(&s, down, &demand(&qr))),
        vec!["<http://ex.org/c>"]
    );
    assert_eq!(
        rows(&run(&s, up, &demand(&qb))),
        vec!["<http://ex.org/a>", "<http://ex.org/b>"]
    );
    assert_eq!(
        rows(&run(&s, up, &demand(&qr))),
        vec!["<http://ex.org/a>", "<http://ex.org/b>"]
    );
}

/// Codex review F17-P1 #3: a schema rule's noValue reads facts that the lower stages
/// of the schema pass complete first.
#[test]
fn schema_negation_reads_complete_lower_stages() {
    let s = store("ex:s ex:p ex:o . ex:x a ex:C .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>.
         [q: (?a ex:p ?b) -> (?a ex:q ?b)]
         [n: (?a ex:p ?b), noValue(?a, ex:q) -> (ex:C rdfs:subClassOf ex:D)]
         [sc: (?c rdfs:subClassOf ?d), (?x rdf:type ?c) -> (?x rdf:type ?d)]"
            .into(),
    );
    // forward materialization fires `n` before `q` has given its fact, as Jena's
    // forward engine does, so the stratified answer is checked on its own
    let qr = reasoner(&rules);
    let q = "SELECT ?x { ?x a ex:D }";
    assert_eq!(rows(&run(&s, q, &demand(&qr))), Vec::<String>::new());
    assert_eq!(
        rows(&run(
            &s,
            "SELECT ?d { ex:C rdfs:subClassOf ?d }",
            &demand(&qr)
        )),
        Vec::<String>::new()
    );
}

/// Codex review F17-P1 #4: the derived facts a query copies into its view are charged
/// to its memory budget, also when they come from cached tables.
#[test]
fn cached_tables_are_charged_to_the_memory_budget() {
    let mut ttl = String::from("ex:C rdfs:subClassOf ex:B .\n");
    for i in 0..20_000 {
        ttl.push_str(&format!("ex:i{i} a ex:C .\n"));
    }
    let s = store(&ttl);
    let qr = reasoner(&Profile::Rdfs);
    let q = "SELECT (COUNT(*) AS ?n) { ?x a ex:B }";
    let warm = run(&s, q, &demand(&qr));
    assert_eq!(
        rows(&warm),
        vec!["\"20000\"^^<http://www.w3.org/2001/XMLSchema#integer>"]
    );
    let mut o = demand(&qr);
    o.max_memory_bytes = Some(1 << 20);
    let r = query(s.snapshot(), q, &o);
    assert!(r.is_err(), "a 1 MiB budget holds 20,000 derived facts");
}

/// Codex review F17-P1 #5: the schema joins of specialization stop at the query's
/// deadline.
#[test]
fn specialization_stops_at_the_deadline() {
    let mut ttl = String::from("ex:a ex:p0 ex:b .\n");
    for i in 0..6000 {
        ttl.push_str(&format!(
            "ex:p{i} rdfs:domain ex:c{i} . ex:q{i} rdfs:range ex:d{i} .\n"
        ));
    }
    let s = store(&ttl);
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>.
         [j: (?p rdfs:domain ?c), (?q rdfs:range ?d), equal(?c, ?d), (?x ?p ?y)
             -> (?x ex:via ?q)]"
            .into(),
    );
    let qr = reasoner(&rules);
    let mut o = demand(&qr);
    o.timeout = Some(std::time::Duration::from_millis(50));
    let t0 = std::time::Instant::now();
    let r = query(s.snapshot(), "SELECT ?q { ex:a ex:via ?q }", &o);
    let took = t0.elapsed();
    assert!(r.is_err(), "the query should time out");
    assert!(
        took < std::time::Duration::from_secs(2),
        "stopped after {took:?}"
    );
}

/// Codex review F17-P1 #6: list builtins ask for the list facts that rules derive.
#[test]
fn list_builtins_ask_for_derived_lists() {
    let s = store("ex:a ex:p ex:b .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [l: (?x ex:p ?y) -> (ex:L rdf:first ?y), (ex:L rdf:rest rdf:nil)]
         [m: (?x ex:p ?y), listMember(ex:L, ?m) -> (?x ex:q ?m)]"
            .into(),
    );
    let qr = reasoner(&rules);
    assert_eq!(
        rows(&run(&s, "SELECT ?m { ex:a ex:q ?m }", &demand(&qr))),
        vec!["<http://ex.org/b>"]
    );
}

/// Codex review F17-P1 #7: a noValue over a constant class asks the stage below for
/// that class alone, so rules stratified by class are not refused.
#[test]
fn negative_type_demands_keep_the_class() {
    let s = store("ex:a ex:p ex:b . ex:c ex:s ex:d .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [l: (?x ex:p ?y) -> (?x rdf:type ex:Lower)]
         [u: (?x ex:p ?y), noValue(?x, rdf:type, ex:Lower) -> (?x rdf:type ex:Upper)]
         [v: (?x ex:s ?y), noValue(?x, rdf:type, ex:Lower) -> (?x rdf:type ex:Upper)]"
            .into(),
    );
    let qr = reasoner(&rules);
    assert_eq!(
        rows(&run(&s, "SELECT ?x { ?x a ex:Upper }", &demand(&qr))),
        vec!["<http://ex.org/c>"]
    );
}

/// Codex review F17-P1 #8: DESCRIBE reads facts outside its pattern, which no demand
/// asks for, so it is refused under demand reasoning.
#[test]
fn describe_is_refused_under_demand() {
    let s = store("ex:a ex:p ex:b .");
    let qr = reasoner(&Profile::Rdfs);
    match query(s.snapshot(), "DESCRIBE ex:a", &demand(&qr)) {
        Err(sparkles_core::Error::Unsupported(m)) => assert!(m.contains("DESCRIBE"), "{m}"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("DESCRIBE should be refused"),
    }
}

/// Codex re-review F17-P1 #1: schema facts that the lower stages of the schema pass
/// derive are kept in the schema's table, so a session that reads the table has them.
#[test]
fn cached_schema_keeps_lower_stage_schema_facts() {
    let s = store("ex:s ex:p ex:o . ex:a a ex:C .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#>.
         [d: (?x ex:p ?y) -> (ex:C rdfs:subClassOf ex:D)]
         [n: (?x ex:p ?y), noValue(ex:C, rdfs:subClassOf, ex:D) -> (ex:C rdfs:subClassOf ex:E)]
         [sc: (?c rdfs:subClassOf ?d), (?x rdf:type ?c) -> (?x rdf:type ?d)]"
            .into(),
    );
    let qr = reasoner(&rules);
    let q = "SELECT ?x { ?x a ex:D }";
    assert_eq!(rows(&run(&s, q, &demand(&qr))), vec!["<http://ex.org/a>"]);
    // derived again from the cached schema
    let mut o = demand(&qr);
    o.no_cache = true;
    assert_eq!(rows(&run(&s, q, &o)), vec!["<http://ex.org/a>"]);
    assert_eq!(
        rows(&run(&s, "SELECT ?x { ?x a ex:E }", &demand(&qr))),
        Vec::<String>::new()
    );
}

/// Codex re-review F17-P1 #2: facts that several cached tables hold are charged once.
#[test]
fn overlapping_tables_are_charged_once() {
    let mut ttl = String::new();
    for i in 0..2000 {
        ttl.push_str(&format!("ex:a{i} ex:p ex:b{i} .\n"));
    }
    let s = store(&ttl);
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [q: (?x ex:q ?y) <- (?x ex:p ?y)]
         [r: (?x ex:r ?y) <- (?x ex:p ?y)]"
            .into(),
    );
    let qr = reasoner(&rules);
    let q = "SELECT (COUNT(*) AS ?n) { ?x ex:q ?y . ?x ex:r ?y }";
    let budget = |nocache: bool| {
        let mut o = demand(&qr);
        o.no_cache = nocache;
        o.max_memory_bytes = Some(2_500_000);
        o
    };
    let want = vec!["\"2000\"^^<http://www.w3.org/2001/XMLSchema#integer>"];
    assert_eq!(rows(&run(&s, q, &budget(true))), want);
    run(&s, q, &demand(&qr));
    assert_eq!(rows(&run(&s, q, &budget(false))), want);
}

/// Codex re-review F17-P1 #3: a rule that reads one stored list and builds another
/// makes the lower stage of derived lists depend on itself, and the program then reads
/// lists as its stage holds them rather than failing as unstratified.
#[test]
fn distinct_list_heads_are_not_a_cycle() {
    let s = store("ex:L0 rdf:first ex:b ; rdf:rest rdf:nil . ex:a ex:p ex:c .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [copy: (?x ex:p ?y), listMember(ex:L0, ?m) -> (ex:L1 rdf:first ?m), (ex:L1 rdf:rest rdf:nil)]
         [use: (?x ex:p ?y), listMember(ex:L0, ?m) -> (?x ex:q ?m)]"
            .into(),
    );
    let qr = reasoner(&rules);
    assert_eq!(
        rows(&run(&s, "SELECT ?m { ex:a ex:q ?m }", &demand(&qr))),
        vec!["<http://ex.org/b>"]
    );
}

/// Second Codex re-review of F17-P1, #2: a noValue on a cycle through a list stays
/// refused when the program is made again without the stage of derived lists.
#[test]
fn negation_through_lists_is_refused() {
    let s = store("ex:a ex:p ex:b .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [l: (?x ex:p ?y), noValue(ex:a, ex:q, ex:b) -> (ex:L rdf:first ex:b), (ex:L rdf:rest rdf:nil)]
         [m: (?x ex:p ?y), listMember(ex:L, ?m) -> (ex:a ex:q ?m)]"
            .into(),
    );
    let qr = reasoner(&rules);
    for q in [
        "SELECT ?m { ex:a ex:q ?m }",
        "SELECT ?m { ex:a ex:q ?m . ex:L rdf:first ?m }",
    ] {
        match query(s.snapshot(), q, &demand(&qr)) {
            Err(sparkles_core::Error::Component(c)) => assert_eq!(c.code, "reasoning-rules"),
            Err(e) => panic!("{q}: {e}"),
            Ok(r) => panic!("{q}: answered {:?}", rows(&r)),
        }
    }
}

/// Third Codex re-review of F17-P1: the check for a noValue on a cycle keeps classes
/// apart, so a class-stratified negation next to a list copy is not refused.
#[test]
fn negation_check_keeps_classes_apart() {
    let s = store("ex:L0 rdf:first ex:b ; rdf:rest rdf:nil . ex:a ex:p ex:c .");
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [copy: (?x ex:p ?y), listMember(ex:L0, ?m) -> (ex:L1 rdf:first ?m), (ex:L1 rdf:rest rdf:nil)]
         [use: (?x ex:p ?y), listMember(ex:L0, ?m), noValue(?x, rdf:type, ex:Lower) -> (?x rdf:type ex:Upper)]"
            .into(),
    );
    let qr = reasoner(&rules);
    assert_eq!(
        rows(&run(&s, "SELECT ?x { ?x a ex:Upper }", &demand(&qr))),
        vec!["<http://ex.org/a>"]
    );
}

/// Fourth Codex re-review of F17-P1: the facts a `listForAll` over a variable list
/// tests are dependencies of the check for a noValue on a cycle.
#[test]
fn negation_through_list_for_all_is_refused() {
    let s = store(
        "ex:a ex:p ex:c . ex:a ex:requires ex:LS .
         ex:LS rdf:first ex:Upper ; rdf:rest rdf:nil .",
    );
    let rules = Profile::Rules(
        "@prefix ex: <http://ex.org/>.
         [u: (?x ex:p ?y), noValue(ex:a, rdf:type, ex:Lower) -> (ex:a rdf:type ex:Upper)]
         [lo: (?z ex:p ?y), listMember(ex:L1, ?x) -> (?x rdf:type ex:Lower)]
         [mk: (?x ex:requires ?ls), listForAll(?ls, ?x, rdf:type) -> (ex:L1 rdf:first ?x), (ex:L1 rdf:rest rdf:nil)]"
            .into(),
    );
    let qr = reasoner(&rules);
    match query(s.snapshot(), "SELECT ?x { ?x a ex:Upper }", &demand(&qr)) {
        Err(sparkles_core::Error::Component(c)) => assert_eq!(c.code, "reasoning-rules"),
        Err(e) => panic!("{e}"),
        Ok(r) => panic!("answered {:?}", rows(&r)),
    }
}

fn derived_schema_profile() -> Profile {
    Profile::Rules(format!(
        "{PREFIXES}
        [middle: (?x ex:seed ?y) -> (?x ex:middle ?y)]
        [instance: (?x ex:middle ?y) -> (?x ex:derived ?y), (?x rdf:type ex:Generated)]
        [class: (?x rdf:type ex:Generated) -> (ex:Generated rdf:type rdfs:Class)]
        [property: (?x ex:derived ?y) -> (ex:derived rdf:type rdf:Property)]
        [domain: (ex:derived rdf:type rdf:Property) -> (ex:derived rdfs:domain ex:Person)]
        [person: (?x ex:derived ?y), (ex:derived rdfs:domain ex:Person) -> (?x rdf:type ex:Person)]
        [stored_class: (?x ex:observed ?y) -> (ex:SeededClass rdf:type rdfs:Class)]
        [stored_property: (?x ex:observed ?y) -> (ex:seededProperty rdf:type rdf:Property)]
        [unrelated: (?x ex:unrelatedSeed ?y) -> (?x ex:unrelatedDerived ?y)]"
    ))
}
const DERIVED_SCHEMA_DATA: &str = "ex:alice ex:seed ex:bob . ex:Existing a rdfs:Class .
    ex:existingProperty a rdf:Property . ex:base ex:observed ex:input .";

#[test]
fn derived_schema_class_property_and_downstream_answers_match_fixed_oracle() {
    let profile = derived_schema_profile();
    let s = store(DERIVED_SCHEMA_DATA);
    let reference = store(DERIVED_SCHEMA_DATA);
    materialize(&reference, &profile, &ReasonOptions::default()).unwrap();
    let qr = reasoner(&profile);
    let cases = [
        (
            "SELECT ?c { ?c a rdfs:Class }",
            vec![
                "<http://ex.org/Existing>",
                "<http://ex.org/Generated>",
                "<http://ex.org/SeededClass>",
            ],
        ),
        (
            "SELECT ?p { ?p a rdf:Property }",
            vec![
                "<http://ex.org/derived>",
                "<http://ex.org/existingProperty>",
                "<http://ex.org/seededProperty>",
            ],
        ),
        (
            "SELECT ?x { ?x a ex:Person }",
            vec!["<http://ex.org/alice>"],
        ),
    ];
    for (q, expected) in cases {
        let expected: Vec<String> = expected.into_iter().map(String::from).collect();
        assert_eq!(
            rows(&run(&reference, q, &materialized())),
            expected,
            "independent materialized oracle: {q}"
        );
        for no_cache in [false, true, false] {
            let mut options = demand(&qr);
            options.no_cache = no_cache;
            assert_eq!(
                rows(&run(&s, q, &options)),
                expected,
                "demand oracle, no_cache={no_cache}: {q}"
            );
        }
    }
}

#[test]
fn derived_schema_equality_reaches_domain_and_instance_answers() {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [middle: (?x ex:seed ?y) -> (?x ex:middle ?y)]
        [edge: (?x ex:middle ?y) -> (?x ex:p ?y)]
        [equality: (?x ex:middle ?y) -> (ex:p owl:sameAs ex:aliasProperty)]
        [domain: (?p owl:sameAs ?q), (?q rdfs:domain ?c) -> (?p rdfs:domain ?c)]
        [instance: (?p rdfs:domain ?c), (?x ?p ?y) -> (?x rdf:type ?c)]"
    ));
    let data = "ex:alice ex:seed ex:bob . ex:aliasProperty rdfs:domain ex:Person .";
    let s = store(data);
    let reference = store(data);
    materialize(&reference, &profile, &ReasonOptions::default()).unwrap();
    let qr = reasoner(&profile);
    let cases = [
        (
            "SELECT ?s ?o { ?s owl:sameAs ?o }",
            vec!["<http://ex.org/p> <http://ex.org/aliasProperty>"],
        ),
        (
            "SELECT ?c { ex:p rdfs:domain ?c }",
            vec!["<http://ex.org/Person>"],
        ),
        (
            "SELECT ?x { ?x a ex:Person }",
            vec!["<http://ex.org/alice>"],
        ),
    ];
    for (q, expected) in cases {
        let expected: Vec<String> = expected.into_iter().map(String::from).collect();
        assert_eq!(rows(&run(&reference, q, &materialized())), expected);
        for _ in 0..2 {
            assert_eq!(rows(&run(&s, q, &demand(&qr))), expected, "{q}");
        }
    }
}

#[test]
fn derived_schema_variable_predicate_producer_declares_its_property() {
    let profile = Profile::Rules(format!("{PREFIXES}
        [name: (?x ex:seed ?y) -> (?x ex:which ex:generatedProperty)]
        [edge: (?x ex:which ?p), (?x ex:seed ?y) -> (?x ?p ?y)]
        [declare: (?x ?p ?y), notEqual(?p, ex:seed), notEqual(?p, ex:which) -> (?p rdf:type rdf:Property)]"));
    let s = store("ex:alice ex:seed ex:bob .");
    let reference = store("ex:alice ex:seed ex:bob .");
    materialize(&reference, &profile, &ReasonOptions::default()).unwrap();
    let q = "SELECT ?p { ?p a rdf:Property FILTER(?p = ex:generatedProperty) }";
    let expected = vec!["<http://ex.org/generatedProperty>"];
    assert_eq!(rows(&run(&reference, q, &materialized())), expected);
    let qr = reasoner(&profile);
    assert_eq!(rows(&run(&s, q, &demand(&qr))), expected);
}

#[test]
fn derived_schema_cache_respects_graph_visibility_and_snapshot_deletion() {
    use sparkles_core::access::{GraphAccess, GraphRule, Graphs};
    use std::sync::Arc;
    let profile = derived_schema_profile();
    let inputs = Inputs {
        data_graphs: vec![
            sparkles_reasoner::GraphRef::Default,
            sparkles_reasoner::GraphRef::Named("http://ex.org/hidden".into()),
        ],
        ..Inputs::default()
    };
    let qr = QueryReasoner::new(profile.clone(), &Extras::default(), inputs).unwrap();
    let s = store(DERIVED_SCHEMA_DATA);
    s.load(&[Source::from_bytes(
        format!("{PREFIXES} GRAPH <http://ex.org/hidden> {{ ex:mallory ex:seed ex:eve . }}")
            .into_bytes(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let q = "SELECT ?x { ?x a ex:Person }";
    assert_eq!(
        rows(&run(&s, q, &demand(&qr))),
        vec!["<http://ex.org/alice>", "<http://ex.org/mallory>"]
    );
    let mut visible = demand(&qr);
    visible.graphs = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["default"], &[])),
        Graphs::none(),
    )));
    for _ in 0..2 {
        assert_eq!(rows(&run(&s, q, &visible)), vec!["<http://ex.org/alice>"]);
    }
    assert!(
        rows(&run(
            &s,
            "SELECT ?x { ?x a ex:Person FILTER(?x = ex:mallory) }",
            &visible
        ))
        .is_empty()
    );
    let pinned = s.snapshot();
    sparkles_core::sparql::update::update(
        &s,
        "DELETE DATA { <http://ex.org/alice> <http://ex.org/seed> <http://ex.org/bob> }",
        &QueryOptions::default(),
    )
    .unwrap();
    assert!(rows(&run(&s, q, &visible)).is_empty());
    assert_eq!(
        rows(&query(pinned, q, &visible).unwrap()),
        vec!["<http://ex.org/alice>"]
    );
}

#[test]
fn derived_schema_selective_query_avoids_unrelated_instance_closure() {
    let mut data = String::from(DERIVED_SCHEMA_DATA);
    for i in 0..1000 {
        data.push_str(&format!("ex:u{i} ex:unrelatedSeed ex:v{i} .\n"));
    }
    let s = store(&data);
    let qr = reasoner(&derived_schema_profile());
    let result = run(&s, "SELECT ?x { ?x a ex:Person }", &demand(&qr));
    assert_eq!(rows(&result), vec!["<http://ex.org/alice>"]);
    assert!(
        result.ctx.reasoning.as_ref().unwrap().report.derived < 100,
        "unrelated instance closure was derived"
    );
    let mut cancelled = demand(&qr);
    cancelled.cancel = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
        true,
    )));
    assert!(matches!(
        query(s.snapshot(), "SELECT ?x { ?x a ex:Person }", &cancelled),
        Err(sparkles_core::Error::Cancelled)
    ));
}

// A capped schema analysis must retain implicit positive dependencies.
#[test]
fn derived_schema_list_for_all_remains_complete_when_abstract_fact_cap_falls_back() {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [middle: (?s ex:seed ?o) -> (?s ex:middle ?o)]
        [edge: (?s ex:middle ?o) -> (?s ex:edge ?o)]
        [schema: (ex:list ex:marker ex:yes), listForAll(ex:list, ex:origin, ex:edge)
          -> (ex:Generated rdf:type rdfs:Class)]"
    ));
    let seed = "ex:origin ex:seed ex:item .
        ex:list ex:marker ex:yes ; rdf:first ex:item ; rdf:rest rdf:nil .";
    let query_text = "SELECT ?c { ?c a rdfs:Class }";
    let expected = vec!["<http://ex.org/Generated>"];
    for padding in [0, 4096] {
        let mut data = seed.to_string();
        for i in 0..padding {
            // Predicates are always kept by the abstraction, so these force its
            // FACTS cap regardless of concrete subject/object abstraction/order.
            data.push_str(&format!("\nex:filler ex:pad{i} ex:value ."));
        }
        let input = store(&data);
        let reference = store(&data);
        materialize(&reference, &profile, &ReasonOptions::default()).unwrap();
        assert_eq!(
            rows(&run(&reference, query_text, &materialized())),
            expected
        );
        let qr = reasoner(&profile);
        for no_cache in [true, false, false] {
            let mut options = demand(&qr);
            options.no_cache = no_cache;
            let result = run(&input, query_text, &options);
            assert_eq!(
                rows(&result),
                expected,
                "listForAll schema consequence, padding={padding}, no_cache={no_cache}"
            );
            assert!(
                result.ctx.reasoning.as_ref().unwrap().report.derived < 100,
                "the selective schema demand must not derive the unrelated padding"
            );
        }
    }
}

fn materialization_implicit_delta(marker: &str, predicate: &str) {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [middle: (?s ex:seed ?o) -> (?s ex:middle ?o)]
        [edge: (?s ex:middle ?o) -> (?s ex:edge ?o)]
        [schema: {marker} listForAll(ex:list, ex:origin, {predicate})
            -> (ex:Generated rdf:type rdfs:Class)]"
    ));
    let reference = store(
        "ex:origin ex:seed ex:item .
        ex:list ex:marker ex:edge ; rdf:first ex:item ; rdf:rest rdf:nil .",
    );
    materialize(&reference, &profile, &ReasonOptions::default()).unwrap();
    assert_eq!(
        rows(&run(
            &reference,
            "SELECT ?c { ?c a rdfs:Class }",
            &materialized()
        )),
        vec!["<http://ex.org/Generated>"]
    );
}

#[test]
fn derived_schema_list_for_all_materialization_constant_delta() {
    materialization_implicit_delta("(ex:list ex:marker ex:edge),", "ex:edge");
}

#[test]
fn derived_schema_list_for_all_materialization_variable_delta() {
    materialization_implicit_delta("(ex:list ex:marker ?p),", "?p");
}

#[test]
fn derived_schema_list_for_all_materialization_without_explicit_atoms() {
    materialization_implicit_delta("", "ex:edge");
}

// An implicit delta must not replay an entirely old generative instance.
#[test]
fn implicit_fact_replay_does_not_refire_a_satisfied_fresh_witness_instance() {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [one: (ex:list ex:marker ?p),
              listForAll(ex:list, ex:origin, ?p), makeTemp(?b)
              -> (?b ex:tag ex:value)]"
    ));
    let reference = store(
        "ex:list ex:marker ex:edge ;
        rdf:first ex:item ; rdf:rest rdf:nil .
        ex:origin ex:edge ex:item .",
    );
    let options = ReasonOptions {
        max_iterations: 8,
        max_inferred: 16,
        ..ReasonOptions::default()
    };
    // There is exactly one successful input binding (?p=ex:edge). All list and
    // edge data already exist before the initial naive round. No new marker,
    // edge or list fact is produced by this program.
    materialize(&reference, &profile, &options)
        .expect("one successful body instance must reach its finite fixpoint");
    let result = run(
        &reference,
        "SELECT ?b { ?b ex:tag ex:value }",
        &materialized(),
    );
    assert_eq!(rows(&result).len(), 1);
}

#[test]
fn implicit_fact_replay_ignores_unrelated_members_of_a_constant_predicate() {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [middle: (ex:start ex:seed ex:yes) -> (ex:start ex:middle ex:yes)]
        [irrelevant: (ex:start ex:middle ex:yes) -> (ex:other ex:edge ex:otherItem)]
        [one: (ex:list ex:marker ex:yes), listForAll(ex:list, ex:origin, ex:edge), makeTemp(?b)
            -> (?b ex:tag ex:value)]"
    ));
    let reference = store(
        "ex:list ex:marker ex:yes ; rdf:first ex:item ; rdf:rest rdf:nil .
        ex:origin ex:edge ex:item . ex:start ex:seed ex:yes .",
    );
    let options = ReasonOptions {
        max_iterations: 8,
        max_inferred: 16,
        ..ReasonOptions::default()
    };
    materialize(&reference, &profile, &options).unwrap();
    assert_eq!(
        rows(&run(
            &reference,
            "SELECT ?b { ?b ex:tag ex:value }",
            &materialized()
        ))
        .len(),
        1
    );
}

#[test]
fn implicit_fact_replay_preserves_new_explicit_bindings_before_a_split() {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [middle: (ex:start ex:seed ex:yes) -> (ex:start ex:middle ex:yes)]
        [new: (ex:start ex:middle ex:yes) -> (ex:new ex:marker ex:edge)]
        [one: (?config ex:marker ?p), (?x ex:allow ex:yes),
            listForAll(ex:list, ex:origin, ?p) -> (?config ex:tag ?x)]"
    ));
    let mut data = String::from(
        "ex:old ex:marker ex:edge .
        ex:list rdf:first ex:item ; rdf:rest rdf:nil .
        ex:origin ex:edge ex:item . ex:start ex:seed ex:yes .",
    );
    for i in 0..10 {
        data.push_str(&format!("ex:allowed{i} ex:allow ex:yes .\n"));
    }
    let reference = store(&data);
    let options = ReasonOptions {
        max_iterations: 8,
        max_inferred: 128,
        ..ReasonOptions::default()
    };
    materialize(&reference, &profile, &options).unwrap();
    // Two marker bindings and ten allow bindings admit exactly twenty facts.
    assert_eq!(
        rows(&run(
            &reference,
            "SELECT ?s ?o { ?s ex:tag ?o }",
            &materialized()
        ))
        .len(),
        20
    );
}

fn fresh_schema_witness_count(mixed: bool) {
    let producer = if mixed {
        "[derived: (?s ex:spawn ?o) -> (?s ex:p ?o)]"
    } else {
        ""
    };
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        {producer}
        [schema: (?s ex:p ?o), makeTemp(?c) -> (?c rdf:type rdfs:Class)]"
    ));
    let data = if mixed {
        "ex:a ex:p ex:b . ex:c ex:spawn ex:d ."
    } else {
        "ex:a ex:p ex:b ."
    };
    let expected_count = if mixed { 2 } else { 1 };
    let input = store(data);
    let reference = store(data);
    materialize(&reference, &profile, &ReasonOptions::default()).unwrap();
    let text = "SELECT ?c { ?c a rdfs:Class }";
    assert_eq!(
        rows(&run(&reference, text, &materialized())).len(),
        expected_count,
        "fixed count of stored and separately derived rule inputs"
    );
    let qr = reasoner(&profile);
    for no_cache in [true, false, false] {
        let mut options = demand(&qr);
        options.no_cache = no_cache;
        assert_eq!(
            rows(&run(&input, text, &options)).len(),
            expected_count,
            "fresh schema count, mixed={mixed}, no_cache={no_cache}"
        );
    }
}

#[test]
fn fresh_schema_stored_inputs_have_one_witness() {
    fresh_schema_witness_count(false);
}

#[test]
fn fresh_schema_mixed_inputs_have_one_witness_each() {
    fresh_schema_witness_count(true);
}

#[test]
fn fresh_schema_multihead_and_downstream_relations_share_witnesses() {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [derived: (?s ex:spawn ?o) -> (?s ex:p ?o)]
        [schema: (?s ex:p ?o), makeTemp(?c)
            -> (?s ex:made ?c), (?c rdf:type rdfs:Class)]
        [downstream: (?c rdf:type rdfs:Class) -> (?c rdfs:subClassOf ex:Root)]"
    ));
    let data = "ex:a ex:p ex:b . ex:c ex:spawn ex:d .";
    let input = store(data);
    let reference = store(data);
    materialize(&reference, &profile, &ReasonOptions::default()).unwrap();
    let joins = "SELECT ?s { ?s ex:made ?c . ?c a rdfs:Class }";
    let downstream = "SELECT ?c { ?c rdfs:subClassOf ex:Root . ?c a rdfs:Class }";
    let expected = vec!["<http://ex.org/a>", "<http://ex.org/c>"];
    assert_eq!(rows(&run(&reference, joins, &materialized())), expected);
    assert_eq!(rows(&run(&reference, downstream, &materialized())).len(), 2);
    let qr = reasoner(&profile);
    for no_cache in [true, false, false] {
        let mut options = demand(&qr);
        options.no_cache = no_cache;
        assert_eq!(rows(&run(&input, joins, &options)), expected);
        assert_eq!(rows(&run(&input, downstream, &options)).len(), 2);
    }
}

#[test]
fn fresh_schema_keeps_complete_negative_lower_stages() {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [flag: (ex:a ex:p ex:b), makeTemp(?c)
            -> (ex:a ex:flag ex:yes), (?c rdf:type rdfs:Class)]
        [upper: (ex:a ex:p ex:b), noValue(ex:a, ex:flag, ex:yes)
            -> (ex:a rdf:type ex:Upper)]"
    ));
    let input = store("ex:a ex:p ex:b .");
    let qr = reasoner(&profile);
    for no_cache in [true, false, false] {
        let mut options = demand(&qr);
        options.no_cache = no_cache;
        assert!(run(&input, "ASK { ex:a ex:flag ex:yes }", &options).boolean);
        // The lower flag is entailed, so the negative premise cannot hold.
        assert!(!run(&input, "ASK { ex:a a ex:Upper }", &options).boolean);
    }
}

#[test]
fn fresh_schema_stable_skolem_terms_are_not_duplicated() {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [schema: (?s ex:p ?o), makeSkolem(?c, ?s, ?o) -> (?c rdf:type rdfs:Class)]"
    ));
    let input = store("ex:a ex:p ex:b .");
    let reference = store("ex:a ex:p ex:b .");
    materialize(&reference, &profile, &ReasonOptions::default()).unwrap();
    let text = "SELECT ?c { ?c a rdfs:Class }";
    assert_eq!(rows(&run(&reference, text, &materialized())).len(), 1);
    let qr = reasoner(&profile);
    for no_cache in [true, false, false] {
        let mut options = demand(&qr);
        options.no_cache = no_cache;
        assert_eq!(rows(&run(&input, text, &options)).len(), 1);
    }
}

#[test]
fn fresh_schema_unreached_generators_keep_result_tables() {
    let profile = Profile::Rules(format!(
        "{PREFIXES}
        [stable: (?s ex:p ?o) -> (?s rdf:type ex:Stable)]
        [unreached: (?s ex:absent ?o), makeTemp(?c) -> (?c rdf:type rdfs:Class)]"
    ));
    let input = store("ex:a ex:p ex:b .");
    let qr = reasoner(&profile);
    let text = "SELECT ?s { ?s a ex:Stable }";
    let first = run(&input, text, &demand(&qr));
    assert_eq!(rows(&first), vec!["<http://ex.org/a>"]);
    assert_eq!(first.ctx.reasoning.as_ref().unwrap().report.cached, 0);
    let second = run(&input, text, &demand(&qr));
    assert_eq!(rows(&second), rows(&first));
    assert_eq!(second.ctx.reasoning.as_ref().unwrap().report.cached, 1);
    let mut options = demand(&qr);
    options.no_cache = true;
    let uncached = run(&input, text, &options);
    assert_eq!(rows(&uncached), rows(&first));
    assert_eq!(uncached.ctx.reasoning.as_ref().unwrap().report.cached, 0);
}

/// Demand and materialization give the same rows for `text` over `ttl` under OWL RL,
/// with the cold schema pass and with its cached table.
fn owl_rl_matches_materialization(ttl: &str, text: &str) {
    let input = store(ttl);
    materialize(&input, &Profile::OwlRl, &ReasonOptions::default()).unwrap();
    let want = rows(&run(&input, text, &materialized()));
    assert!(!want.is_empty(), "the oracle has answers for {text}");
    let qr = reasoner(&Profile::OwlRl);
    for no_cache in [true, false, false] {
        let mut options = demand(&qr);
        options.no_cache = no_cache;
        assert_eq!(
            rows(&run(&input, text, &options)),
            want,
            "{text} (no_cache {no_cache})"
        );
    }
}

/// An instance whose class is a subclass of `owl:Class` is derived to be a class. The
/// derived declaration does not hide the instance's other types, here one that class
/// equality gives.
#[test]
fn derived_meta_type_keeps_equal_class_instances() {
    owl_rl_matches_materialization(
        "ex:C2 owl:sameAs ex:C0 .
         ex:i0 a ex:C2 .
         ex:C2 rdfs:subClassOf owl:Class .",
        "SELECT ?a { ?a rdf:type ex:C0 }",
    );
}

/// A class equivalence read through a subproperty chain types both classes by the
/// range, while one of them is also derived to be a class of classes.
#[test]
fn derived_meta_type_keeps_range_types_of_schema_terms() {
    owl_rl_matches_materialization(
        "ex:C1 owl:equivalentClass ex:C3 .
         ex:C1 rdfs:subClassOf owl:Class .
         owl:equivalentClass rdfs:subPropertyOf ex:p0 .
         ex:p0 rdfs:range ex:C3 .",
        "SELECT ?a { ?a rdf:type ex:C3 }",
    );
}

/// A triple of a small view that is mostly schema: W3C classes of classes and
/// properties as types, superclasses, domains and ranges, schema predicates as
/// subproperties and as subjects of `rdfs:subPropertyOf`, and `owl:sameAs` between
/// classes, properties and individuals.
fn schema_heavy_triple(r: &mut Rng) -> String {
    let c = |r: &mut Rng| format!("ex:C{}", r.below(4));
    let p = |r: &mut Rng| format!("ex:p{}", r.below(4));
    let i = |r: &mut Rng| format!("ex:i{}", r.below(6));
    let meta = |r: &mut Rng| {
        [
            "owl:Class",
            "owl:TransitiveProperty",
            "owl:SymmetricProperty",
            "rdf:Property",
            "owl:FunctionalProperty",
            "owl:ObjectProperty",
            "rdfs:Class",
        ][r.below(7)]
        .to_string()
    };
    let schema = |r: &mut Rng| {
        [
            "rdfs:subClassOf",
            "rdfs:subPropertyOf",
            "rdf:type",
            "owl:sameAs",
            "rdfs:domain",
            "rdfs:range",
            "owl:inverseOf",
            "owl:equivalentClass",
            "owl:equivalentProperty",
        ][r.below(9)]
        .to_string()
    };
    let any = |r: &mut Rng| match r.below(3) {
        0 => format!("ex:C{}", r.below(4)),
        1 => format!("ex:p{}", r.below(4)),
        _ => format!("ex:i{}", r.below(6)),
    };
    match r.below(20) {
        0 => format!("{} rdfs:subClassOf {}", c(r), c(r)),
        1 => format!("{} rdfs:subPropertyOf {}", p(r), p(r)),
        2 => format!("{} rdfs:domain {}", p(r), c(r)),
        3 => format!("{} rdfs:range {}", p(r), c(r)),
        4 | 16 => format!("{} a {}", i(r), c(r)),
        5 | 6 => format!("{} {} {}", i(r), p(r), any(r)),
        7 => format!("{} owl:sameAs {}", any(r), any(r)),
        8 => format!("{} rdfs:range {}", p(r), meta(r)),
        9 => format!("{} rdfs:domain {}", p(r), meta(r)),
        10 => format!("{} rdfs:subPropertyOf {}", p(r), schema(r)),
        11 => format!("{} rdfs:subClassOf {}", c(r), meta(r)),
        12 => format!("{} a {}", any(r), meta(r)),
        13 => format!("{} owl:inverseOf {}", p(r), p(r)),
        14 => format!("{} owl:equivalentProperty {}", p(r), p(r)),
        15 => format!("{} owl:equivalentClass {}", c(r), c(r)),
        17 => format!("{} {} {}", any(r), p(r), any(r)),
        18 => format!("{} rdfs:subPropertyOf {}", schema(r), p(r)),
        _ => format!("{} {} {}", any(r), schema(r), any(r)),
    }
}

/// A one-pattern query over the terms of [`schema_heavy_triple`].
fn schema_heavy_pattern(r: &mut Rng) -> String {
    let term = |r: &mut Rng, v: &str| -> String {
        match r.below(5) {
            0 | 1 => format!("?{v}"),
            2 => format!("ex:C{}", r.below(4)),
            3 => format!("ex:p{}", r.below(4)),
            _ => format!("ex:i{}", r.below(6)),
        }
    };
    let p = match r.below(14) {
        0 => "?pp".to_string(),
        1..=4 => format!("ex:p{}", r.below(4)),
        5 => "rdfs:subClassOf".into(),
        6 => "rdfs:subPropertyOf".into(),
        7 => "owl:sameAs".into(),
        8 => "rdfs:domain".into(),
        9 => "owl:inverseOf".into(),
        _ => "rdf:type".into(),
    };
    let s = term(r, "a");
    let o = if p == "rdf:type" && r.below(2) == 0 {
        [
            "owl:Class",
            "owl:TransitiveProperty",
            "rdf:Property",
            "owl:SymmetricProperty",
            "rdfs:Class",
            "owl:FunctionalProperty",
        ][r.below(6)]
        .to_string()
    } else {
        term(r, "b")
    };
    format!("{s} {p} {o}")
}

/// For small schema-heavy views, the solutions under `demand` equal those over an OWL RL
/// materialization. Some of these views make equality expand into a large closure, so
/// each query has a deadline: a query past it is counted and skipped, and the others
/// must agree.
#[test]
fn schema_heavy_demand_answers_like_materialization() {
    let seeds: Vec<u64> = match std::env::var("SCHEMA_HEAVY_SEEDS") {
        Ok(v) => v.split(',').map(|s| s.trim().parse().unwrap()).collect(),
        Err(_) => SCHEMA_HEAVY_SEEDS.to_vec(),
    };
    let deadline = std::time::Duration::from_secs(5);
    let (mut checked, mut skipped) = (0usize, 0usize);
    for seed in seeds {
        let mut r = Rng(seed * 6_700_417 + 3);
        let mut ttl = String::new();
        for _ in 0..25 {
            ttl.push_str(&format!("{} .\n", schema_heavy_triple(&mut r)));
        }
        let s = store(&ttl);
        materialize(&s, &Profile::OwlRl, &ReasonOptions::default())
            .unwrap_or_else(|e| panic!("seed {seed}: {e:#}"));
        let qr = reasoner(&Profile::OwlRl);
        for _ in 0..8 {
            let q = format!("SELECT DISTINCT * {{ {} }}", schema_heavy_pattern(&mut r));
            let want = rows(&run(&s, &q, &materialized()));
            let mut options = demand(&qr);
            options.timeout = Some(deadline);
            match query(s.snapshot(), &q, &options) {
                Ok(got) => {
                    checked += 1;
                    assert_eq!(rows(&got), want, "seed {seed}: {q}\n{ttl}");
                }
                Err(sparkles_core::Error::Timeout) => skipped += 1,
                Err(e) => panic!("seed {seed}: {q}: {e}\n{ttl}"),
            }
        }
    }
    eprintln!("schema-heavy differential: {checked} queries agree, {skipped} past the deadline");
    assert!(
        checked >= 2 * skipped,
        "{checked} checked, {skipped} skipped"
    );
}

/// The seeds of [`schema_heavy_demand_answers_like_materialization`]: views whose queries
/// finish within the deadline in a development build, among them 16 and 25, which found
/// the derived meta type cases above. `SCHEMA_HEAVY_SEEDS=1,2,3` runs others.
const SCHEMA_HEAVY_SEEDS: &[u64] = &[1, 3, 9, 15, 16, 17, 18, 24, 25];
