//! Graph views with the statistics shortcuts: counts answered from the index statistics
//! through a view equal the counts of a store that holds only the view's graphs, before
//! and after a delta, and their plans name no quads of hidden graphs.

use super::opt_tests::{PREFIXES, has_desc, has_op, load, solutions, update};
use super::*;
use crate::access::{GraphAccess, GraphRule, Graphs};
use crate::io::RdfFormat;
use crate::store::{Store, StoreOptions};
use std::sync::Arc;

/// Typed subjects in the default graph and in graphs `ex:a` and `ex:b` (`keep` picks
/// which), compacted.
fn typed(keep: &dyn Fn(&str) -> bool) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from("@prefix ex: <http://ex.org/> .\n");
    for (g, n, m) in [("", 4000, 7), ("a", 3000, 5), ("b", 2000, 11)] {
        if !keep(g) {
            continue;
        }
        let open = if g.is_empty() {
            String::new()
        } else {
            format!("ex:{g} {{\n")
        };
        ttl.push_str(&open);
        for i in 0..n {
            ttl.push_str(&format!(
                "ex:s{i} a ex:C{} ; ex:p{} ex:o{} .\n",
                i % m,
                i % 3,
                i % 17
            ));
        }
        if !g.is_empty() {
            ttl.push_str("}\n");
        }
    }
    load(&s, &ttl, RdfFormat::TriG);
    s.compact().unwrap();
    s
}

fn delta(s: &Store, keep: &dyn Fn(&str) -> bool) {
    let mut ins = String::new();
    if keep("") {
        ins.push_str("ex:new a ex:C1 , ex:Fresh . ");
    }
    if keep("a") {
        ins.push_str("GRAPH ex:a { ex:s1 a ex:C9 . ex:new2 a ex:C2 } ");
    }
    if keep("b") {
        ins.push_str("GRAPH ex:b { ex:s1 a ex:Hidden . ex:s2 a ex:C1 } ");
    }
    if !ins.is_empty() {
        update(s, &format!("INSERT DATA {{ {ins} }}"));
    }
}

fn view(names: &[&str]) -> Option<Arc<GraphAccess>> {
    let r = Graphs::Only(GraphRule::new(names, &[]));
    Some(Arc::new(GraphAccess {
        read: r.clone(),
        write: r,
        triples: None,
    }))
}

#[test]
fn statistics_shortcuts_through_a_view() {
    let _unlimited = Unlimited::on();
    type Keep = fn(&str) -> bool;
    let views: [(&[&str], Keep); 3] = [
        (&["default", "http://ex.org/a"], |g| g != "b"),
        (&["http://ex.org/a"], |g| g == "a"),
        (&["http://ex.org/*"], |g| !g.is_empty()),
    ];
    let queries = [
        "SELECT ?t (COUNT(?s) AS ?c) WHERE { ?s a ?t } GROUP BY ?t",
        "SELECT ?t (COUNT(?s) AS ?c) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s a ?t } } GROUP BY ?t",
        "SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p",
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { ?s ?p ?o }",
        "SELECT (COUNT(DISTINCT ?o) AS ?c) WHERE { GRAPH <urn:x-arq:UnionGraph> { ?s ?p ?o } }",
        "SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }",
        "SELECT (COUNT(*) AS ?c) WHERE { GRAPH ?g { ?s a ?t } }",
    ];
    for (names, keep) in views {
        let full = typed(&|_| true);
        let expected = typed(&keep);
        let mut from_statistics = 0;
        for step in 0..2 {
            if step == 1 {
                delta(&full, &|_| true);
                delta(&expected, &keep);
            }
            for q in queries {
                let q = format!("{PREFIXES}{q}");
                let opts = QueryOptions {
                    graphs: view(names),
                    no_cache: true,
                    ..Default::default()
                };
                let got = query(full.snapshot(), &q, &opts).unwrap();
                let want = query(
                    expected.snapshot(),
                    &q,
                    &QueryOptions {
                        no_cache: true,
                        ..Default::default()
                    },
                )
                .unwrap();
                assert_eq!(
                    solutions(&got),
                    solutions(&want),
                    "{names:?} step {step}: {q}"
                );
                assert!(!has_desc(&got.plan, "graphs not read"), "{:#?}", got.plan);
                assert!(!has_desc(&got.plan, "delta quads"), "{:#?}", got.plan);
                if has_op(&got.plan, "GroupCountFromMetadata")
                    || has_desc(&got.plan, "[from statistics]")
                {
                    from_statistics += 1;
                }
            }
        }
        assert!(
            from_statistics > 0,
            "{names:?}: no count came from the statistics"
        );
    }
}

/// Lift the work limit of the statistics' corrections while alive.
struct Unlimited;

impl Unlimited {
    fn on() -> Unlimited {
        super::stats::UNLIMITED.with(|u| u.set(true));
        Unlimited
    }
}

impl Drop for Unlimited {
    fn drop(&mut self) {
        super::stats::UNLIMITED.with(|u| u.set(false));
    }
}
