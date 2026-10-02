//! Triple-level protections (`GraphAccess::triples`): a query through a view answers
//! exactly what the same query answers on a store that holds only the triples the view
//! sees. The data, the protections, the grants that lift them and the caller are random;
//! what a view sees is worked out here, independently of the engine, from the rules'
//! definitions. Writes of protected triples fail before anything changes, whether or not
//! the triples exist.

use sparkles::access::{
    Caller, GraphAccess, GraphRule, Graphs, Limits, Protection, Rule, TripleRules,
};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::update::update;
use sparkles::sparql::{QueryKind, QueryOptions, query};
use sparkles::store::{Store, StoreOptions};
use sparkles::{Error, history::At};
use std::collections::BTreeSet;
use std::sync::Arc;

const INFERRED: &str = "urn:x-sparkles:inferred";
const EX: &str = "http://ex/";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const SUB_CLASS_OF: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
const LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";

const SPARQL_PREFIXES: &str = "PREFIX ex: <http://ex/> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
    PREFIX spk: <urn:x-sparkles:> \
    PREFIX text: <http://jena.apache.org/text#> \
    PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
    PREFIX geof: <http://www.opengis.net/def/function/geosparql/> ";

/// A quad as N-Quads terms: subject, predicate, object, graph (`""`: the default graph).
type Q = (String, String, String, String);

fn iri(x: &str) -> String {
    if x.starts_with("http") || x.starts_with("urn:") {
        format!("<{x}>")
    } else {
        format!("<{EX}{x}>")
    }
}

fn lit(x: &str) -> String {
    format!("\"{x}\"")
}

fn int(n: usize) -> String {
    format!("\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>")
}

fn nquads(qs: &[Q]) -> String {
    qs.iter()
        .map(|(s, p, o, g)| {
            if g.is_empty() {
                format!("{s} {p} {o} .\n")
            } else {
                format!("{s} {p} {o} {g} .\n")
            }
        })
        .collect()
}

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

const GRAPHS: [&str; 5] = [
    "",
    "http://ex/a/1",
    "http://ex/a/2",
    "http://ex/b/1",
    INFERRED,
];
const USERS: [&str; 2] = ["alice", "bob"];
const ROLES: [&str; 2] = ["hr", "staff"];

/// A random triple about one of eight subjects.
fn random_triple(r: &mut Rng) -> (String, String, String) {
    let s = iri(&format!("s{}", r.below(8)));
    let (p, o) = match r.below(12) {
        0 => (iri("p"), iri(&format!("s{}", r.below(8)))),
        1 => (iri("q"), lit(r.pick(&["x", "y", "secret"]))),
        2 => (iri("n"), int(r.below(5))),
        3 => (
            iri(LABEL),
            lit(r.pick(&["fox one", "fox two", "brown fox", "quiet hen"])),
        ),
        4 => (iri("salary"), int(1000 * (1 + r.below(4)))),
        5 => (iri("owner"), lit(r.pick(&USERS))),
        6 => (iri("team"), iri(r.pick(&["t1", "t2"]))),
        7 => (iri("visibleTo"), lit(r.pick(&ROLES))),
        8 => (
            iri("emb"),
            format!(
                "\"{}\"^^<urn:x-sparkles:vector>",
                r.pick(&["[1, 0, 0]", "[0.9, 0.1, 0]", "[0, 1, 0]"])
            ),
        ),
        9 => (
            iri("http://www.opengis.net/ont/geosparql#asWKT"),
            format!(
                "\"POINT({} {})\"^^<http://www.opengis.net/ont/geosparql#wktLiteral>",
                r.below(4),
                r.below(4)
            ),
        ),
        _ => (iri(RDF_TYPE), iri(r.pick(&["C1", "C2", "C3", "C4"]))),
    };
    (s, p, o)
}

/// Random data: the base (compacted) and a delta of inserts and deletes over it.
fn random_data(r: &mut Rng) -> (Vec<Q>, Vec<Q>, Vec<Q>) {
    let mut base: BTreeSet<Q> = BTreeSet::new();
    // a class hierarchy, team membership and a switch for the ASK-like pattern
    base.insert((iri("C3"), iri(SUB_CLASS_OF), iri("C1"), String::new()));
    base.insert((
        iri("C4"),
        iri(SUB_CLASS_OF),
        iri("C3"),
        iri("http://ex/a/2"),
    ));
    base.insert((iri("t1"), iri("member"), lit("alice"), String::new()));
    base.insert((iri("t2"), iri("member"), lit("bob"), iri("http://ex/b/1")));
    if r.chance(50) {
        base.insert((iri("cfg"), iri("open"), lit("yes"), String::new()));
    }
    for g in GRAPHS {
        let g = if g.is_empty() { String::new() } else { iri(g) };
        for _ in 0..14 {
            let (s, p, o) = random_triple(r);
            base.insert((s, p, o, g.clone()));
        }
    }
    let base: Vec<Q> = base.into_iter().collect();
    let mut ins = BTreeSet::new();
    for _ in 0..10 {
        let (s, p, o) = random_triple(r);
        let g = *r.pick(&GRAPHS);
        let g = if g.is_empty() { String::new() } else { iri(g) };
        let q = (s, p, o, g);
        if !base.contains(&q) {
            ins.insert(q);
        }
    }
    let mut del = BTreeSet::new();
    for _ in 0..6 {
        del.insert(r.pick(&base).clone());
    }
    (base, ins.into_iter().collect(), del.into_iter().collect())
}

fn store_of(base: &[Q], ins: &[Q], del: &[Q], union: bool) -> Store {
    let s = Store::in_memory(StoreOptions {
        union_default_graph: union,
        result_cache_min_ms: 0.0,
        ..Default::default()
    });
    if !base.is_empty() {
        s.load(&[Source::from_bytes(
            nquads(base).into_bytes(),
            RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    }
    s.compact().unwrap();
    #[cfg(feature = "text")]
    s.enable_text(sparkles::text::TextConfig::default())
        .unwrap();
    #[cfg(feature = "geo")]
    s.enable_geo(sparkles::geo::GeoConfig::default()).unwrap();
    let block = |qs: &[Q]| -> String {
        qs.iter()
            .map(|(s, p, o, g)| {
                if g.is_empty() {
                    format!("{s} {p} {o} . ")
                } else {
                    format!("GRAPH {g} {{ {s} {p} {o} }} ")
                }
            })
            .collect()
    };
    if !ins.is_empty() {
        update(
            &s,
            &format!("INSERT DATA {{ {} }}", block(ins)),
            &QueryOptions::default(),
        )
        .unwrap();
    }
    if !del.is_empty() {
        update(
            &s,
            &format!("DELETE DATA {{ {} }}", block(del)),
            &QueryOptions::default(),
        )
        .unwrap();
    }
    s
}

/// The final quads of the random data.
fn final_quads(base: &[Q], ins: &[Q], del: &[Q]) -> Vec<Q> {
    let mut all: BTreeSet<Q> = base.iter().cloned().collect();
    all.extend(ins.iter().cloned());
    for d in del {
        all.remove(d);
    }
    all.into_iter().collect()
}

/// A protection and how the test evaluates its pattern.
#[derive(Clone)]
struct Prot {
    p: Protection,
    /// the pattern's solutions on `quads` for `caller`, as (s, p, o) where a column the
    /// pattern does not bind is `None`; `Err(true/false)` for a pattern without them
    eval: Option<
        fn(&[Q], &Caller) -> Result<Vec<(Option<String>, Option<String>, Option<String>)>, bool>,
    >,
}

fn prot(name: &str) -> Protection {
    Protection {
        name: name.into(),
        predicates: None,
        classes: None,
        subclasses: true,
        graphs: None,
        pattern: None,
        prefixes: [("ex".to_string(), EX.to_string())].into(),
        hide_inferences: false,
    }
}

fn s_of(qs: &[Q], f: impl Fn(&Q) -> bool) -> Vec<(Option<String>, Option<String>, Option<String>)> {
    qs.iter()
        .filter(|q| f(q))
        .map(|q| (Some(q.0.clone()), None, None))
        .collect()
}

/// The protections the tests draw from.
fn pool() -> Vec<Prot> {
    let mut out = Vec::new();
    let mut p = prot("salaries");
    p.predicates = Some(vec![format!("{EX}salary")]);
    out.push(Prot { p, eval: None });
    let mut p = prot("q-glob");
    p.predicates = Some(vec![format!("{EX}q*")]);
    p.graphs = Some(Graphs::Only(GraphRule::new(
        ["default", "http://ex/a/*"],
        &[],
    )));
    out.push(Prot { p, eval: None });
    let mut p = prot("c1");
    p.classes = Some(vec![format!("{EX}C1")]);
    out.push(Prot { p, eval: None });
    let mut p = prot("c2-direct");
    p.classes = Some(vec![format!("{EX}C2")]);
    p.subclasses = false;
    p.predicates = Some(vec![
        format!("{EX}n"),
        LABEL.to_string(),
        format!("{EX}emb"),
    ]);
    out.push(Prot { p, eval: None });
    let mut p = prot("owned");
    p.classes = Some(vec![format!("{EX}C2")]);
    p.pattern = Some("?s ex:owner ?user".into());
    out.push(Prot {
        p,
        eval: Some(|qs, c| {
            let Some(u) = &c.user else { return Ok(vec![]) };
            Ok(s_of(qs, |q| q.1 == iri("owner") && q.2 == lit(u)))
        }),
    });
    let mut p = prot("team");
    p.predicates = Some(vec![format!("{EX}p")]);
    p.pattern = Some("?s ex:team ?t . ?t ex:member ?user".into());
    out.push(Prot {
        p,
        eval: Some(|qs, c| {
            let Some(u) = &c.user else { return Ok(vec![]) };
            let teams: BTreeSet<&String> = qs
                .iter()
                .filter(|q| q.1 == iri("member") && q.2 == lit(u))
                .map(|q| &q.0)
                .collect();
            Ok(s_of(qs, |q| q.1 == iri("team") && teams.contains(&q.2)))
        }),
    });
    let mut p = prot("switch");
    p.predicates = Some(vec![LABEL.to_string()]);
    p.pattern = Some("ex:cfg ex:open \"yes\"".into());
    out.push(Prot {
        p,
        eval: Some(|qs, _| {
            Err(qs
                .iter()
                .any(|q| q.0 == iri("cfg") && q.1 == iri("open") && q.2 == lit("yes")))
        }),
    });
    let mut p = prot("not-secret");
    p.predicates = Some(vec![format!("{EX}q")]);
    p.pattern = Some("?s ex:q ?o FILTER(?o != \"secret\")".into());
    out.push(Prot {
        p,
        eval: Some(|qs, _| {
            Ok(qs
                .iter()
                .filter(|q| q.1 == iri("q") && q.2 != lit("secret"))
                .map(|q| (Some(q.0.clone()), None, Some(q.2.clone())))
                .collect())
        }),
    });
    let mut p = prot("role");
    p.predicates = Some(vec![format!("{EX}salary"), format!("{EX}team")]);
    p.pattern = Some("?this ex:visibleTo ?role".into());
    out.push(Prot {
        p,
        eval: Some(|qs, c| {
            Ok(s_of(qs, |q| {
                q.1 == iri("visibleTo") && c.roles.iter().any(|r| q.2 == lit(r))
            }))
        }),
    });
    let mut p = prot("everything-owned");
    p.pattern = Some("?s ex:owner ?user".into());
    p.graphs = Some(Graphs::Only(GraphRule::new(
        ["http://ex/b/*", INFERRED],
        &[],
    )));
    out.push(Prot {
        p,
        eval: Some(|qs, c| {
            let Some(u) = &c.user else { return Ok(vec![]) };
            Ok(s_of(qs, |q| q.1 == iri("owner") && q.2 == lit(u)))
        }),
    });
    let mut p = prot("n-in-a");
    p.predicates = Some(vec![format!("{EX}n")]);
    p.graphs = Some(Graphs::Only(GraphRule::new(["http://ex/a/*"], &[])));
    out.push(Prot { p, eval: None });
    out
}

/// Random graph sets for lifts and reads.
fn random_graphs(r: &mut Rng) -> Graphs {
    match r.below(6) {
        0 | 1 => Graphs::none(),
        2 => Graphs::All,
        3 => Graphs::Only(GraphRule::new(["default"], &[INFERRED])),
        4 => Graphs::Only(GraphRule::new(["http://ex/a/*"], &[INFERRED])),
        _ => Graphs::Only(GraphRule::new(
            ["default", "http://ex/b/1", INFERRED],
            &[INFERRED],
        )),
    }
}

fn term_of(g: &str) -> Option<oxrdf::Term> {
    if g.is_empty() {
        None
    } else {
        Some(oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            g.trim_start_matches('<').trim_end_matches('>'),
        )))
    }
}

/// The quads a view sees, from the definitions of the rules.
fn visible(quads: &[Q], read: &Graphs, rules: &[(Prot, Graphs)], caller: &Caller) -> Vec<Q> {
    let in_force: Vec<&(Prot, Graphs)> = rules.iter().filter(|(_, l)| !l.is_all()).collect();
    let hide_inferred = in_force.iter().any(|(p, _)| p.p.hide_inferences);
    let read = if hide_inferred {
        read.without_iri(INFERRED)
    } else {
        read.clone()
    };
    // class members, over every graph
    let types: Vec<(&String, &String)> = quads
        .iter()
        .filter(|q| q.1 == iri(RDF_TYPE))
        .map(|q| (&q.0, &q.2))
        .collect();
    let subs: Vec<(&String, &String)> = quads
        .iter()
        .filter(|q| q.1 == iri(SUB_CLASS_OF))
        .map(|q| (&q.0, &q.2))
        .collect();
    let members = |classes: &[String], sub: bool| -> BTreeSet<String> {
        let mut closure: BTreeSet<String> = classes.iter().map(|c| iri(c)).collect();
        if sub {
            loop {
                let more: Vec<String> = subs
                    .iter()
                    .filter(|(_, sup)| closure.contains(*sup))
                    .map(|(c, _)| (*c).clone())
                    .filter(|c| !closure.contains(c))
                    .collect();
                if more.is_empty() {
                    break;
                }
                closure.extend(more);
            }
        }
        types
            .iter()
            .filter(|(_, c)| closure.contains(*c))
            .map(|(s, _)| (*s).clone())
            .collect()
    };
    let prepared: Vec<_> = in_force
        .iter()
        .map(|(p, lift)| {
            let m = p.p.classes.as_ref().map(|c| members(c, p.p.subclasses));
            let e = p.eval.map(|f| f(quads, caller));
            (p, lift, m, e)
        })
        .collect();
    quads
        .iter()
        .filter(|q| {
            let g = term_of(&q.3);
            if !read.allows(g.as_ref()) {
                return false;
            }
            let pred = q.1.trim_start_matches('<').trim_end_matches('>');
            !prepared.iter().any(|(p, lift, m, e)| {
                let covered = p.p.covers_predicate(pred)
                    && p.p.covers_graph(g.as_ref())
                    && m.as_ref().is_none_or(|m| m.contains(&q.0));
                let passes = match e {
                    None => false,
                    Some(Err(all)) => *all,
                    Some(Ok(sols)) => sols.iter().any(|(s, p2, o)| {
                        s.as_ref().is_none_or(|s| *s == q.0)
                            && p2.as_ref().is_none_or(|p2| *p2 == q.1)
                            && o.as_ref().is_none_or(|o| *o == q.2)
                    }),
                };
                covered && !lift.allows(g.as_ref()) && !passes
            })
        })
        .cloned()
        .collect()
}

fn answer(s: &Store, q: &str, opts: &QueryOptions) -> Vec<String> {
    let r = query(s.snapshot(), &format!("{SPARQL_PREFIXES}{q}"), opts)
        .unwrap_or_else(|e| panic!("{q}: {e}"));
    let mut out: Vec<String> = match r.kind {
        QueryKind::Select => r
            .rows()
            .into_iter()
            .map(|row| {
                row.iter()
                    .map(|t| t.as_ref().map_or("-".to_string(), |t| t.to_string()))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect(),
        QueryKind::Ask => vec![r.boolean.to_string()],
        _ => r.triples.iter().map(|t| t.to_string()).collect(),
    };
    out.sort();
    out
}

const QUERIES: &[&str] = &[
    "SELECT * { ?s ?p ?o }",
    "SELECT * { GRAPH ?g { ?s ?p ?o } }",
    "SELECT ?g { GRAPH ?g { } }",
    "SELECT (COUNT(*) AS ?n) { ?s ?p ?o }",
    "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } }",
    "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ex:salary ?o } }",
    "SELECT (COUNT(*) AS ?n) { ?s ex:salary ?o }",
    "SELECT (COUNT(DISTINCT ?s) AS ?n) { GRAPH ?g { ?s ?p ?o } }",
    "SELECT (COUNT(DISTINCT ?s) AS ?n) { ?s ?p ?o }",
    "SELECT (COUNT(DISTINCT ?o) AS ?n) { GRAPH ?g { ?s ex:p ?o } }",
    "SELECT (COUNT(DISTINCT ?s) AS ?n) { GRAPH ?g { ?s ex:salary ?o } }",
    "SELECT (COUNT(*) AS ?n) { ?s a ?c }",
    "SELECT ?c (COUNT(?s) AS ?n) { GRAPH ?g { ?s a ?c } } GROUP BY ?c",
    "SELECT ?c (COUNT(?s) AS ?n) { ?s a ?c } GROUP BY ?c",
    "SELECT ?c (COUNT(DISTINCT ?s) AS ?n) { GRAPH ?g { ?s a ?c } } GROUP BY ?c",
    "SELECT ?p (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?p",
    "SELECT ?p (COUNT(*) AS ?n) { ?s ?p ?o } GROUP BY ?p",
    "SELECT ?g (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?g",
    "SELECT ?s (SUM(?n) AS ?t) { GRAPH ?g { ?s ex:salary ?n } } GROUP BY ?s",
    "SELECT (AVG(?n) AS ?a) (MAX(?n) AS ?m) { GRAPH ?g { ?s ex:salary ?n } }",
    "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ex:p ?o . ?o ex:p ?x } }",
    "SELECT (COUNT(*) AS ?n) { ?s ex:p ?o . ?o ex:p ?x }",
    "SELECT ?s ?o { ?s ex:p+ ?o }",
    "SELECT ?g ?s ?o { GRAPH ?g { ?s ex:p+ ?o } }",
    "SELECT ?s ?o { GRAPH ?g { ?s ex:p/ex:p ?o } }",
    "SELECT ?s ?o { GRAPH ?g { ?s (ex:p|ex:team)* ?o } }",
    "SELECT ?s ?o FROM <http://ex/a/1> FROM <http://ex/b/1> { ?s ex:p* ?o }",
    "SELECT ?s ?o FROM <http://ex/b/1> { ?s ex:salary ?o }",
    "SELECT ?g ?s FROM NAMED <http://ex/b/1> FROM NAMED <http://ex/a/2> { GRAPH ?g { ?s a ?c } }",
    "ASK { GRAPH ?g { ?s ex:salary ?o } }",
    "ASK { ?s ex:q \"secret\" }",
    "ASK { GRAPH <http://ex/b/1> { } }",
    "SELECT * { GRAPH <http://ex/b/1> { ?s ?p ?o } }",
    "SELECT * { GRAPH <urn:x-sparkles:inferred> { ?s ?p ?o } }",
    "SELECT * { GRAPH <urn:x-arq:UnionGraph> { ?s ex:p ?o } }",
    "SELECT (COUNT(*) AS ?n) { GRAPH <urn:x-arq:UnionGraph> { ?s ?p ?o } }",
    "DESCRIBE ex:s1",
    "DESCRIBE ?s WHERE { GRAPH ?g { ?s a ex:C1 } }",
    "CONSTRUCT { ?s ?p ?o } WHERE { GRAPH ?g { ?s ?p ?o } }",
    "SELECT ?s { GRAPH ?g { ?s a ex:C1 } FILTER EXISTS { GRAPH ?h { ?s ex:salary ?x } } }",
    "SELECT ?s { GRAPH ?g { ?s ?p ?o } FILTER NOT EXISTS { GRAPH ?h { ?s ex:q ?x } } }",
    "SELECT ?s { GRAPH ?g { ?s a ?c } MINUS { GRAPH ?h { ?s a ex:C2 } } }",
    "SELECT ?s ?x { GRAPH ?g { ?s a ?c } OPTIONAL { GRAPH ?h { ?s ex:salary ?x } } }",
    "SELECT ?s { { SELECT ?s (COUNT(*) AS ?n) { GRAPH ?g { ?s ?p ?o } } GROUP BY ?s } FILTER(?n > 1) }",
    "SELECT ?s ?o { GRAPH ?g { ?s ex:p ?o } } ORDER BY ?o ?s LIMIT 3",
    "SELECT ?s ?n { GRAPH ?g { ?s ex:salary ?n } } ORDER BY DESC(?n) ?s LIMIT 2",
    "SELECT DISTINCT ?s { GRAPH ?g { ?s ex:p ?o } FILTER(STRSTARTS(STR(?o), \"http://ex/s\")) }",
    "SELECT (COUNT(*) AS ?n) { GRAPH ?g { ?s ex:q ?o } FILTER(STRSTARTS(?o, \"se\")) }",
    "SELECT ?s ?v { VALUES ?s { ex:s0 ex:s1 ex:s2 } GRAPH ?g { ?s ?p ?v } }",
    "SELECT ?s ?score { GRAPH ?g { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 30 \"exact:true\") } }",
    "SELECT ?s { (?s ?score) spk:vectorSearch (ex:emb \"[1,0,0]\"^^spk:vector 30 \"exact:true\") }",
    #[cfg(feature = "text")]
    "SELECT ?s ?lit { GRAPH ?g { (?s ?sc ?lit) text:query \"fox\" } }",
    #[cfg(feature = "text")]
    "SELECT ?s ?lit { (?s ?sc ?lit) text:query \"fox\" }",
    #[cfg(feature = "text")]
    "SELECT DISTINCT ?s { GRAPH ?g { (?s ?score) spk:hybridSearch ((rdfs:label \"fox\") (ex:emb \"[1,0,0]\"^^spk:vector)) } }",
    #[cfg(feature = "geo")]
    "SELECT ?f { GRAPH ?g { ?f geo:asWKT ?w } \
       FILTER(geof:sfWithin(?w, \"POLYGON((0.5 0.5, 10 0.5, 10 10, 0.5 10, 0.5 0.5))\"^^geo:wktLiteral)) }",
    #[cfg(feature = "geo")]
    "SELECT ?f { ?f geo:asWKT ?w \
       FILTER(geof:sfWithin(?w, \"POLYGON((0.5 0.5, 10 0.5, 10 10, 0.5 10, 0.5 0.5))\"^^geo:wktLiteral)) }",
];

/// One random case: the data, the view's rules and the caller.
struct Case {
    base: Vec<Q>,
    ins: Vec<Q>,
    del: Vec<Q>,
    read: Graphs,
    rules: Vec<(Prot, Graphs)>,
    caller: Caller,
}

fn random_case(seed: u64) -> Case {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let (base, ins, del) = random_data(&mut r);
    let pool = pool();
    let n = 1 + r.below(3);
    let mut rules = Vec::new();
    let mut used = BTreeSet::new();
    for _ in 0..n {
        let i = r.below(pool.len());
        if !used.insert(i) {
            continue;
        }
        let mut p = pool[i].clone();
        p.p.hide_inferences = r.chance(30);
        rules.push((p, random_graphs(&mut r)));
    }
    let read = match r.below(4) {
        0 => Graphs::Only(GraphRule::new(
            ["default", "http://ex/a/*", INFERRED],
            &[INFERRED],
        )),
        _ => Graphs::All,
    };
    let caller = Caller {
        user: match r.below(3) {
            0 => None,
            i => Some(USERS[i - 1].to_string()),
        },
        roles: ROLES
            .iter()
            .filter(|_| r.chance(50))
            .map(|x| x.to_string())
            .collect(),
        groups: Vec::new(),
    };
    Case {
        base,
        ins,
        del,
        read,
        rules,
        caller,
    }
}

fn access_of(c: &Case) -> Arc<GraphAccess> {
    let rules = TripleRules {
        rules: c
            .rules
            .iter()
            .map(|(p, lift)| Rule {
                protection: Arc::new(p.p.clone()),
                read: lift.clone(),
                write: lift.clone(),
            })
            .collect(),
        caller: c.caller.clone(),
        limits: Limits::default(),
    };
    Arc::new(GraphAccess::with_triples(
        c.read.clone(),
        c.read.clone(),
        rules,
    ))
}

fn differential(seed: u64, union: bool) {
    let c = random_case(seed);
    let full = store_of(&c.base, &c.ins, &c.del, union);
    let quads = final_quads(&c.base, &c.ins, &c.del);
    let seen = visible(&quads, &c.read, &c.rules, &c.caller);
    let oracle = store_of(&seen, &[], &[], union);
    let restricted = QueryOptions {
        graphs: Some(access_of(&c)),
        ..Default::default()
    };
    let names: Vec<&str> = c.rules.iter().map(|(p, _)| p.p.name.as_str()).collect();
    for q in QUERIES {
        if union && q.starts_with("DESCRIBE") {
            continue;
        }
        // the result cache holds the full answer first; the view must not read it
        let _ = answer(&full, q, &QueryOptions::default());
        let want = answer(&oracle, q, &QueryOptions::default());
        let got = answer(&full, q, &restricted);
        assert_eq!(
            got, want,
            "seed {seed}, union {union}, rules {names:?}, caller {:?}: {q}",
            c.caller
        );
        assert_eq!(
            answer(&full, q, &restricted),
            want,
            "cached, seed {seed}: {q}"
        );
    }
}

#[test]
fn a_view_answers_like_a_store_of_its_visible_triples() {
    for seed in 0..24 {
        differential(seed, false);
    }
}

#[test]
fn a_view_answers_like_a_store_of_its_visible_triples_with_a_union_default_graph() {
    for seed in 100..112 {
        differential(seed, true);
    }
}

/// The statistics shortcuts answer counts on a masked snapshot exactly, with their work
/// limit lifted as in the graph-view tests: here, through a view whose rules hide whole
/// predicates and classes.
#[test]
fn counts_from_statistics_through_a_mask() {
    let c = random_case(7);
    let full = store_of(&c.base, &c.ins, &c.del, false);
    let mut p = prot("salaries");
    p.predicates = Some(vec![format!("{EX}salary")]);
    let mut k = prot("c1");
    k.classes = Some(vec![format!("{EX}C1")]);
    let rules = vec![
        (Prot { p, eval: None }, Graphs::none()),
        (Prot { p: k, eval: None }, Graphs::none()),
    ];
    let case = Case {
        rules,
        read: Graphs::All,
        caller: Caller::default(),
        ..c
    };
    let quads = final_quads(&case.base, &case.ins, &case.del);
    let oracle = store_of(
        &visible(&quads, &case.read, &case.rules, &case.caller),
        &[],
        &[],
        false,
    );
    let opts = QueryOptions {
        graphs: Some(access_of(&case)),
        ..Default::default()
    };
    for q in [
        "SELECT ?c (COUNT(?s) AS ?n) { ?s a ?c } GROUP BY ?c",
        "SELECT ?p (COUNT(*) AS ?n) { ?s ?p ?o } GROUP BY ?p",
        "SELECT (COUNT(DISTINCT ?s) AS ?n) { ?s ?p ?o }",
        "SELECT (COUNT(DISTINCT ?o) AS ?n) { ?s ?p ?o }",
        "SELECT (COUNT(DISTINCT ?s) AS ?n) { ?s ex:p ?o }",
        "SELECT (COUNT(*) AS ?n) { ?s ?p ?o }",
    ] {
        assert_eq!(
            answer(&full, q, &opts),
            answer(&oracle, q, &QueryOptions::default()),
            "{q}"
        );
    }
}

fn salaries() -> Protection {
    let mut p = prot("salaries");
    p.predicates = Some(vec![format!("{EX}salary")]);
    p
}

fn patients() -> Protection {
    let mut p = prot("patients");
    p.classes = Some(vec![format!("{EX}Patient")]);
    p
}

fn owned() -> Protection {
    let mut p = prot("owned");
    p.classes = Some(vec![format!("{EX}Doc")]);
    p.pattern = Some("?s ex:owner ?user".into());
    p
}

fn fixture() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        r#"
        <http://ex/alice> <http://ex/salary> "100"^^<http://www.w3.org/2001/XMLSchema#integer> .
        <http://ex/alice> <http://ex/name> "Alice" .
        <http://ex/bob> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/InPatient> .
        <http://ex/bob> <http://ex/name> "Bob" .
        <http://ex/InPatient> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex/Patient> .
        <http://ex/d1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/Doc> .
        <http://ex/d1> <http://ex/owner> "carol" .
        <http://ex/d1> <http://ex/title> "Carol's" .
        <http://ex/d2> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/Doc> .
        <http://ex/d2> <http://ex/owner> "dave" .
        <http://ex/d2> <http://ex/title> "Dave's" .
        <http://ex/bob> <http://ex/salary> "200"^^<http://www.w3.org/2001/XMLSchema#integer> <http://ex/g> .
        "#
        .as_bytes()
        .to_vec(),
        RdfFormat::NQuads,
        None,
    )])
    .unwrap();
    s
}

fn writer(protections: &[(Protection, Graphs, Graphs)], user: &str) -> QueryOptions {
    let rules = TripleRules {
        rules: protections
            .iter()
            .map(|(p, read, write)| Rule {
                protection: Arc::new(p.clone()),
                read: read.clone(),
                write: write.clone(),
            })
            .collect(),
        caller: Caller {
            user: Some(user.into()),
            ..Default::default()
        },
        limits: Limits::default(),
    };
    QueryOptions {
        graphs: Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            rules,
        ))),
        ..Default::default()
    }
}

fn refused(s: &Store, u: &str, opts: &QueryOptions) -> String {
    let head = s.head_commit().seq;
    match update(s, &format!("{SPARQL_PREFIXES}{u}"), opts) {
        Err(Error::NotPermitted(m)) => {
            assert_eq!(s.head_commit().seq, head, "{u}: nothing may change");
            m
        }
        r => panic!("{u}: expected a refusal, got {:?}", r.map(|_| ())),
    }
}

fn ok(s: &Store, u: &str, opts: &QueryOptions) {
    update(s, &format!("{SPARQL_PREFIXES}{u}"), opts).unwrap_or_else(|e| panic!("{u}: {e}"));
}

/// Writes of protected triples fail before anything changes, with the same answer
/// whether or not the triple exists; the rest of the write goes through.
#[test]
fn writes_of_protected_triples_fail_whether_or_not_they_exist() {
    let s = fixture();
    let none = Graphs::none();
    let staff = writer(
        &[
            (salaries(), none.clone(), none.clone()),
            (patients(), none.clone(), none.clone()),
        ],
        "eve",
    );
    // a salary: present or not, the same refusal
    let a = refused(&s, "INSERT DATA { ex:alice ex:salary 100 }", &staff);
    let b = refused(&s, "INSERT DATA { ex:alice ex:salary 999 }", &staff);
    assert!(a.starts_with("write access to the triple"), "{a}");
    assert!(!a.contains("salaries") && !b.contains("salaries"), "{a}");
    refused(&s, "DELETE DATA { ex:alice ex:salary 100 }", &staff);
    refused(&s, "DELETE DATA { ex:alice ex:salary 12345 }", &staff);
    // terms the store lacks: refused all the same
    refused(&s, "DELETE DATA { ex:nobody ex:salary 777 }", &staff);
    // a patient (through the subclass) is protected; a new one may not be made
    refused(&s, "DELETE DATA { ex:bob ex:name \"Bob\" }", &staff);
    refused(&s, "DELETE DATA { ex:bob ex:name \"not there\" }", &staff);
    refused(&s, "INSERT DATA { ex:bob ex:note \"x\" }", &staff);
    refused(
        &s,
        "INSERT DATA { ex:zed a ex:Patient ; ex:name \"Zed\" }",
        &staff,
    );
    // nor may the class hierarchy below a protected class change
    refused(
        &s,
        "DELETE DATA { ex:InPatient rdfs:subClassOf ex:Patient }",
        &staff,
    );
    refused(
        &s,
        "INSERT DATA { ex:Other rdfs:subClassOf ex:InPatient }",
        &staff,
    );
    // the WHERE clause reads through the view: hidden triples match nothing
    ok(&s, "DELETE WHERE { ?s ex:salary ?o }", &staff);
    ok(
        &s,
        "DELETE { ?s ?p ?o } WHERE { ?s ex:name \"Bob\" ; ?p ?o }",
        &staff,
    );
    let all = answer(
        &s,
        "SELECT * { { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }",
        &QueryOptions::default(),
    );
    assert!(all.iter().any(|r| r.contains("salary")), "{all:?}");
    assert!(all.iter().any(|r| r.contains("\"Bob\"")), "{all:?}");
    // the rest is writable
    ok(&s, "INSERT DATA { ex:alice ex:name \"Al\" }", &staff);
    // CLEAR acts on the triples the view sees and leaves the hidden ones, but may not
    // drop the subclass link that makes bob a patient
    refused(&s, "CLEAR DEFAULT", &staff);
    ok(&s, "CLEAR GRAPH <http://ex/g>", &staff);
    let left = answer(
        &s,
        "SELECT ?s ?p { GRAPH <http://ex/g> { ?s ?p ?o } }",
        &QueryOptions::default(),
    );
    assert_eq!(left, ["<http://ex/bob> <http://ex/salary>"]);
    let dropped = writer(&[(salaries(), none.clone(), none.clone())], "eve");
    ok(&s, "CLEAR DEFAULT", &dropped);
    let left = answer(&s, "SELECT ?s ?p { ?s ?p ?o }", &QueryOptions::default());
    assert_eq!(left, ["<http://ex/alice> <http://ex/salary>"]);
    // a dry run is refused the same way
    let mut dry = staff.clone();
    dry.write.dry_run = Some(Default::default());
    refused(&s, "INSERT DATA { ex:alice ex:salary 1 }", &dry);
}

/// A grant that lifts a protection for writing lets its holder write those triples; a
/// pattern lets the owner write their own documents and nobody else's.
#[test]
fn lifted_protections_and_patterns_allow_writes() {
    let s = fixture();
    let all = Graphs::All;
    let hr = writer(&[(salaries(), all.clone(), all.clone())], "hr");
    ok(&s, "INSERT DATA { ex:alice ex:salary 101 }", &hr);
    // read but not write: still refused
    let reader = writer(&[(salaries(), all.clone(), Graphs::none())], "x");
    refused(&s, "INSERT DATA { ex:alice ex:salary 102 }", &reader);
    // lifted in one graph only
    let g = Graphs::Only(GraphRule::new(["http://ex/g"], &[]));
    let in_g = writer(&[(salaries(), g.clone(), g.clone())], "x");
    ok(
        &s,
        "INSERT DATA { GRAPH <http://ex/g> { ex:alice ex:salary 5 } }",
        &in_g,
    );
    refused(&s, "INSERT DATA { ex:alice ex:salary 5 }", &in_g);
    // owners
    let none = Graphs::none();
    let carol = writer(&[(owned(), none.clone(), none.clone())], "carol");
    ok(
        &s,
        "INSERT DATA { ex:d1 ex:title \"Carol's, revised\" }",
        &carol,
    );
    refused(&s, "INSERT DATA { ex:d2 ex:title \"Mine now\" }", &carol);
    // taking a document over is a write to it
    refused(&s, "INSERT DATA { ex:d2 ex:owner \"carol\" }", &carol);
    // a new document of one's own
    ok(
        &s,
        "INSERT DATA { ex:d3 a ex:Doc ; ex:owner \"carol\" ; ex:title \"New\" }",
        &carol,
    );
    // a new document of someone else's is not visible to its writer afterwards: refused
    refused(
        &s,
        "INSERT DATA { ex:d4 a ex:Doc ; ex:owner \"dave\" }",
        &carol,
    );
    let seen = answer(&s, "SELECT ?d ?t { ?d ex:title ?t }", &carol);
    assert_eq!(
        seen,
        [
            "<http://ex/d1> \"Carol's\"",
            "<http://ex/d1> \"Carol's, revised\"",
            "<http://ex/d3> \"New\""
        ]
    );
}

/// Graph Store replaces keep the triples the view hides, and may not drop a protected
/// class's hierarchy.
#[test]
fn replacing_a_graph_keeps_hidden_triples() {
    let s = fixture();
    let none = Graphs::none();
    let replace = |opts: &QueryOptions| {
        s.replace_with(
            sparkles::store::ReplaceTarget::Default,
            &[Source::from_bytes(
                b"<http://ex/new> <http://ex/name> \"New\" .".to_vec(),
                RdfFormat::NTriples,
                None,
            )],
            sparkles::commit::CommitKind::Transaction,
            &sparkles::guard::WriteOptions {
                graphs: opts.graphs.clone(),
                ..Default::default()
            },
        )
    };
    // the default graph holds the subclass link that makes bob a patient
    let care = writer(&[(patients(), none.clone(), none.clone())], "x");
    assert!(matches!(replace(&care), Err(Error::NotPermitted(_))));
    let carol = writer(
        &[
            (salaries(), none.clone(), none.clone()),
            (owned(), none.clone(), none.clone()),
        ],
        "carol",
    );
    replace(&carol).unwrap();
    let left = answer(&s, "SELECT ?s ?p { ?s ?p ?o }", &QueryOptions::default());
    // alice's salary and dave's document were hidden and stay; carol's document and
    // bob's triples were visible and are gone
    assert_eq!(
        left,
        [
            "<http://ex/alice> <http://ex/salary>",
            "<http://ex/d2> <http://ex/owner>",
            "<http://ex/d2> <http://ex/title>",
            "<http://ex/d2> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type>",
            "<http://ex/new> <http://ex/name>",
        ]
    );
}

/// Materialized inferences can restate hidden facts: a rule that hides them hides the
/// whole inferred graph.
#[test]
fn hidden_inferences() {
    let s = fixture();
    ok(
        &s,
        "INSERT DATA { GRAPH <urn:x-sparkles:inferred> { ex:alice a ex:WellPaid } }",
        &QueryOptions::default(),
    );
    let mut p = salaries();
    p.hide_inferences = true;
    let staff = writer(&[(p, Graphs::none(), Graphs::none())], "x");
    let q = "SELECT ?c { GRAPH ?g { ex:alice a ?c } }";
    assert_eq!(answer(&s, q, &staff), Vec::<String>::new());
    assert_eq!(
        answer(&s, q, &QueryOptions::default()),
        ["<http://ex/WellPaid>"]
    );
    // a rule lifted everywhere is not in force, and hides nothing
    let mut p = salaries();
    p.hide_inferences = true;
    let hr = writer(&[(p, Graphs::All, Graphs::All)], "x");
    assert!(hr.graphs.as_ref().unwrap().triples.is_none());
    assert_eq!(answer(&s, q, &hr), ["<http://ex/WellPaid>"]);
}

/// Plans read through a mask carry no estimates (they come from statistics of every
/// triple), and the full view plans as without one.
#[test]
fn plans_through_a_mask_carry_no_estimates() {
    let s = fixture();
    let staff = writer(&[(salaries(), Graphs::none(), Graphs::none())], "x");
    let q = format!("{SPARQL_PREFIXES}SELECT * {{ ?s ex:salary ?o }}");
    let (_, plan) = sparkles::sparql::explain(s.snapshot(), &q, &staff).unwrap();
    assert_eq!(plan.estimated_rows, -1.0);
    // lifted everywhere: no rules, the same plan as without a view
    let hr = writer(&[(salaries(), Graphs::All, Graphs::All)], "x");
    let (_, a) = sparkles::sparql::explain(s.snapshot(), &q, &hr).unwrap();
    let (_, b) = sparkles::sparql::explain(s.snapshot(), &q, &QueryOptions::default()).unwrap();
    assert_eq!(a.estimated_rows, b.estimated_rows);
}

/// The masked snapshot is built once per commit and view.
#[test]
fn masks_are_kept_per_commit() {
    let s = fixture();
    let staff = writer(&[(salaries(), Graphs::none(), Graphs::none())], "x");
    let a = staff.graphs.as_ref().unwrap();
    let snap = s.snapshot();
    let m1 = a.masked(&snap).unwrap();
    let m2 = a.masked(&snap).unwrap();
    assert!(Arc::ptr_eq(&m1, &m2));
    assert_eq!(m1.mask.as_ref().unwrap().hidden.len(), 2);
    ok(
        &s,
        "INSERT DATA { ex:carol ex:salary 3 }",
        &QueryOptions::default(),
    );
    let m3 = a.masked(&s.snapshot()).unwrap();
    assert_eq!(m3.mask.as_ref().unwrap().hidden.len(), 3);
}

/// Diffs show the changes of the triples the view sees; with protections that depend on
/// the data, a triple whose visibility changed counts as added or removed.
#[test]
fn diffs_through_a_view() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let mut dump = Vec::new();
    fixture().dump_nquads(&mut dump).unwrap();
    s.load(&[Source::from_bytes(dump, RdfFormat::NQuads, None)])
        .unwrap();
    let c0 = s.head_commit().seq;
    ok(
        &s,
        "INSERT DATA { ex:carol ex:salary 3 ; ex:name \"Carol\" . ex:alice a ex:Patient }",
        &QueryOptions::default(),
    );
    let c1 = s.head_commit().seq;
    let diff = |opts: &QueryOptions| -> Vec<String> {
        let d = s
            .diff(
                &At::Commit(c0),
                &At::Commit(c1),
                &sparkles::store::DiffOptions {
                    graphs: opts.graphs.clone(),
                    ..Default::default()
                },
            )
            .unwrap();
        let mut v: Vec<String> = d
            .iter()
            .map(|(op, q)| format!("{} {q}", op.sign()))
            .collect();
        v.sort();
        v
    };
    let none = Graphs::none();
    let pay = writer(&[(salaries(), none.clone(), none.clone())], "x");
    assert_eq!(
        diff(&pay),
        [
            "+ <http://ex/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://ex/Patient>",
            "+ <http://ex/carol> <http://ex/name> \"Carol\""
        ]
    );
    // alice became a patient: her name disappears from the view
    let care = writer(&[(patients(), none.clone(), none.clone())], "x");
    assert_eq!(
        diff(&care),
        [
            "+ <http://ex/carol> <http://ex/name> \"Carol\"",
            "+ <http://ex/carol> <http://ex/salary> \"3\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            "- <http://ex/alice> <http://ex/name> \"Alice\"",
            "- <http://ex/alice> <http://ex/salary> \"100\"^^<http://www.w3.org/2001/XMLSchema#integer>",
        ]
    );
}

/// Schema reports count the triples the view sees.
#[test]
fn schema_reports_through_a_view() {
    let s = fixture();
    let none = Graphs::none();
    let staff = writer(&[(salaries(), none.clone(), none.clone())], "x");
    let opts = sparkles::schema::SchemaOptions {
        graph: sparkles::schema::GraphSelection::Union,
        graphs: staff.graphs.clone(),
        ..Default::default()
    };
    let r = sparkles::schema::discover(&s.snapshot(), &opts).unwrap();
    let text = format!("{:?}", r.predicates);
    assert!(!text.contains("salary"), "{text}");
    let full = sparkles::schema::discover(
        &s.snapshot(),
        &sparkles::schema::SchemaOptions {
            graph: sparkles::schema::GraphSelection::Union,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(format!("{:?}", full.predicates).contains("salary"));
}

/// A view may hide only so many quads at one commit.
#[test]
fn the_hidden_quads_have_a_limit() {
    let s = fixture();
    let mut rules = TripleRules {
        rules: vec![Rule {
            protection: Arc::new(salaries()),
            read: Graphs::none(),
            write: Graphs::none(),
        }],
        caller: Caller::default(),
        limits: Limits::default(),
    };
    rules.limits.max_hidden = 1;
    let opts = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            rules,
        ))),
        ..Default::default()
    };
    match query(s.snapshot(), "SELECT * { ?s ?p ?o }", &opts) {
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, sparkles::BudgetKind::HiddenQuads),
        r => panic!("{:?}", r.map(|_| ())),
    }
}

/// RDFS on read derives from the triples a view sees: a type that only a hidden triple
/// implies is not derived.
#[test]
fn rdfs_on_read_derives_from_visible_triples() {
    use sparkles::sparql::rdfs::{RdfsOnRead, RdfsSchema};
    let s = fixture();
    let n = |x: &str| oxrdf::NamedNode::new_unchecked(x);
    let schema = RdfsSchema::from_triples(&[
        oxrdf::Triple::new(
            n("http://ex/salary"),
            n("http://www.w3.org/2000/01/rdf-schema#domain"),
            n("http://ex/Employee"),
        ),
        oxrdf::Triple::new(
            n("http://ex/name"),
            n("http://www.w3.org/2000/01/rdf-schema#domain"),
            n("http://ex/Person"),
        ),
    ]);
    let rdfs = Some(Arc::new(RdfsOnRead::fixed(schema)));
    let q = "SELECT ?s ?c { ?s a ?c FILTER(?c IN (ex:Employee, ex:Person)) }";
    let full = answer(
        &s,
        q,
        &QueryOptions {
            rdfs: rdfs.clone(),
            ..Default::default()
        },
    );
    assert!(
        full.contains(&"<http://ex/alice> <http://ex/Employee>".to_string()),
        "{full:?}"
    );
    let mut staff = writer(&[(salaries(), Graphs::none(), Graphs::none())], "x");
    staff.rdfs = rdfs;
    assert_eq!(
        answer(&s, q, &staff),
        [
            "<http://ex/alice> <http://ex/Person>",
            "<http://ex/bob> <http://ex/Person>"
        ]
    );
}
