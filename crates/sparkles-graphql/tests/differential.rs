//! GraphQL answers against the equivalent SPARQL on random small graphs: filters (and,
//! or, not, nested object filters), orders with the id tie-break, slices, nested lists
//! and multi-valued fields.

use oxrdf::Term;
use serde_json::{Value as J, json};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_graphql::{Compiled, Config, Options, Request, execute_on};

const EX: &str = "http://example.org/";

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

const NAMES: [&str; 4] = ["Ann", "Bob", "Cy", "Dee"];

fn graph(r: &mut Rng) -> String {
    let n = 5 + r.below(20);
    let mut s = format!("@prefix ex: <{EX}> .\n");
    for i in 0..n {
        if r.below(5) > 0 {
            s.push_str(&format!("ex:n{i:02} a ex:T .\n"));
        }
        for _ in 0..r.below(3) {
            s.push_str(&format!(
                "ex:n{i:02} ex:name \"{}\" .\n",
                NAMES[r.below(4) as usize]
            ));
        }
        if r.below(3) > 0 {
            s.push_str(&format!("ex:n{i:02} ex:age {} .\n", r.below(60)));
        }
        for _ in 0..r.below(4) {
            s.push_str(&format!("ex:n{i:02} ex:knows ex:n{:02} .\n", r.below(n)));
        }
    }
    s
}

/// A filter as GraphQL input and as a SPARQL expression on `?n`.
fn filter(r: &mut Rng, depth: u32, k: &mut usize) -> (J, String) {
    *k += 1;
    let v = format!("?v{k}");
    let choice = if depth == 0 { r.below(4) } else { r.below(7) };
    match choice {
        0 => {
            let a = r.below(60);
            let (op, sop) =
                [("gt", ">"), ("lt", "<"), ("eq", "="), ("gte", ">=")][r.below(4) as usize];
            (
                json!({ "age": { op: a } }),
                format!("EXISTS {{ ?n ex:age {v} FILTER({v} {sop} {a}) }}"),
            )
        }
        1 => {
            let name = NAMES[r.below(4) as usize];
            (
                json!({ "name": { "eq": name } }),
                format!(
                    "EXISTS {{ ?n ex:name {v} FILTER(isLiteral({v}) && STR({v}) = \"{name}\") }}"
                ),
            )
        }
        2 => {
            let x = r.below(2) == 0;
            let p = format!("EXISTS {{ ?n ex:age {v} }}");
            (
                json!({ "age": { "exists": x } }),
                if x { p } else { format!("!({p})") },
            )
        }
        3 => {
            let names: Vec<&str> = (0..2).map(|_| NAMES[r.below(4) as usize]).collect();
            (
                json!({ "name": { "in": names } }),
                format!(
                    "EXISTS {{ ?n ex:name {v} FILTER(isLiteral({v}) && STR({v}) IN (\"{}\", \"{}\")) }}",
                    names[0], names[1]
                ),
            )
        }
        4 => {
            let (f, e) = filter(r, depth - 1, k);
            (json!({ "not": f }), format!("!({e})"))
        }
        5 => {
            let (f, e) = filter(r, depth - 1, k);
            let (g, h) = filter(r, depth - 1, k);
            (json!({ "or": [f, g] }), format!("(({e}) || ({h}))"))
        }
        _ => {
            let a = r.below(60);
            *k += 1;
            let m = format!("?m{k}");
            (
                json!({ "knows": { "age": { "lt": a } } }),
                format!("EXISTS {{ ?n ex:knows {m} . {m} ex:age {v} FILTER({v} < {a}) }}"),
            )
        }
    }
}

fn ids(r: &sparkles_core::sparql::QueryResult) -> Vec<String> {
    r.rows()
        .into_iter()
        .filter_map(|row| match &row[0] {
            Some(Term::NamedNode(n)) => Some(n.as_str().to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn graphql_answers_equal_sparql() {
    let sdl = format!(
        "extend schema @rdf(vocab: \"{EX}\") @prefix(name: \"ex\", iri: \"{EX}\")\ntype T {{ name: [String!]! age: Int knows: [T!]! }}\n"
    );
    let (c, _) = Compiled::new(Config::new(sdl), 1, &|_, _, _| true)
        .unwrap_or_else(|e| panic!("{}", e.message()));
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut cases = 0;
    for _ in 0..40 {
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[Source::from_bytes(
                graph(&mut rng).into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        for _ in 0..10 {
            let mut k = 0;
            let (f, e) = filter(&mut rng, 2, &mut k);
            let desc = rng.below(2) == 0;
            let order = if rng.below(2) == 0 {
                Some(if desc { "AGE_DESC" } else { "AGE_ASC" })
            } else {
                None
            };
            let first = 1 + rng.below(8);
            let offset = rng.below(4);
            let gq = format!(
                "query($f: TFilter) {{ allT(filter: $f, first: {first}, offset: {offset}{}) {{ totalCount nodes {{ id name knows {{ id }} }} }} }}",
                order
                    .map(|o| format!(", orderBy: [{o}]"))
                    .unwrap_or_default()
            );
            let r = execute_on(
                &c,
                store.snapshot(),
                &Request {
                    query: gq.clone(),
                    variables: serde_json::from_value(json!({ "f": f })).unwrap(),
                    ..Default::default()
                },
                &Options::default(),
            );
            assert!(r.body.get("errors").is_none(), "{gq} {f}: {:#}", r.body);
            let got = &r.body["data"]["allT"];
            let sort = match order {
                Some(_) if desc => "ORDER BY DESC(?o) ?n",
                Some(_) => "ORDER BY ?o ?n",
                None => "ORDER BY ?n",
            };
            let base = format!(
                "PREFIX ex: <{EX}> SELECT ?n WHERE {{ {{ SELECT DISTINCT ?n WHERE {{ ?n a ex:T FILTER({e}) }} }} OPTIONAL {{ ?n ex:age ?o }} }} {sort}"
            );
            let all = sparkles_core::sparql::query(store.snapshot(), &base, &Default::default())
                .unwrap_or_else(|err| panic!("{base}: {err}"));
            let all = ids(&all);
            assert_eq!(got["totalCount"], all.len(), "{gq} {f}\n{base}");
            let want: Vec<String> = all
                .iter()
                .skip(offset as usize)
                .take(first as usize)
                .cloned()
                .collect();
            let have: Vec<String> = got["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["id"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(have, want, "{gq} {f}\n{base}");
            // nested lists: the names and the people known, in the engine's term order
            for n in got["nodes"].as_array().unwrap() {
                let id = n["id"].as_str().unwrap();
                let q = format!("SELECT DISTINCT ?m WHERE {{ <{id}> <{EX}knows> ?m }} ORDER BY ?m");
                let knows =
                    ids(
                        &sparkles_core::sparql::query(store.snapshot(), &q, &Default::default())
                            .unwrap(),
                    );
                let have: Vec<String> = n["knows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| k["id"].as_str().unwrap().to_string())
                    .collect();
                assert_eq!(have, knows, "{id}");
                let q = format!("SELECT DISTINCT ?v WHERE {{ <{id}> <{EX}name> ?v }} ORDER BY ?v");
                let names: Vec<String> =
                    sparkles_core::sparql::query(store.snapshot(), &q, &Default::default())
                        .unwrap()
                        .rows()
                        .into_iter()
                        .filter_map(|row| match &row[0] {
                            Some(Term::Literal(l)) => Some(l.value().to_string()),
                            _ => None,
                        })
                        .collect();
                assert_eq!(n["name"], json!(names), "{id}");
            }
            cases += 1;
        }
    }
    assert_eq!(cases, 400);
}
