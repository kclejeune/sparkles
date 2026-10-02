//! The generated data of the reasoner benchmarks.

use std::fmt::Write as _;

/// ~`n` triples: a class tree (fan-out 4, depth 5), a property hierarchy with domains
/// and ranges, typed instances and property assertions.
pub fn generate(n: usize) -> String {
    const EX: &str = "http://bench.example.org/";
    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const SUBCLASS: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
    const SUBPROP: &str = "http://www.w3.org/2000/01/rdf-schema#subPropertyOf";
    const DOMAIN: &str = "http://www.w3.org/2000/01/rdf-schema#domain";
    const RANGE: &str = "http://www.w3.org/2000/01/rdf-schema#range";
    let mut out = String::with_capacity(n * 90);
    let mut t = |s: &str, p: &str, o: &str| {
        let _ = writeln!(out, "<{s}> <{p}> {o} .");
    };
    // class tree
    let mut classes = vec![0usize];
    let mut next = 1;
    let mut level = vec![0usize];
    for _ in 0..5 {
        let mut nl = Vec::new();
        for &c in &level {
            for _ in 0..4 {
                t(&format!("{EX}C{next}"), SUBCLASS, &format!("<{EX}C{c}>"));
                classes.push(next);
                nl.push(next);
                next += 1;
            }
        }
        level = nl;
    }
    let leaves = level;
    // properties: 40, each with a parent among the first 10, domains/ranges on some
    for p in 0..40 {
        if p >= 10 {
            t(&format!("{EX}p{p}"), SUBPROP, &format!("<{EX}p{}>", p % 10));
        }
        if p % 3 == 0 {
            t(
                &format!("{EX}p{p}"),
                DOMAIN,
                &format!("<{EX}C{}>", classes[p % classes.len()]),
            );
        }
        if p % 4 == 0 {
            t(
                &format!("{EX}p{p}"),
                RANGE,
                &format!("<{EX}C{}>", classes[(p * 7) % classes.len()]),
            );
        }
    }
    // instances: each has a leaf type, 3 object properties and 1 literal
    let inst = n / 5;
    for i in 0..inst {
        let s = format!("{EX}i{i}");
        t(
            &s,
            RDF_TYPE,
            &format!("<{EX}C{}>", leaves[i % leaves.len()]),
        );
        for k in 0..3 {
            let p = (i * 7 + k * 13) % 40;
            let o = (i * 31 + k * 17 + 1) % inst;
            t(&s, &format!("{EX}p{p}"), &format!("<{EX}i{o}>"));
        }
        t(&s, &format!("{EX}name"), &format!("\"instance {i}\""));
    }
    out
}
