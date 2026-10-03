//! The cost of RDFS on read: queries over stored triples, over the RDFS closure computed
//! at query time, and over materialized inferences, on the generated benchmark data
//! (its schema is in the same default graph).
//!
//! ```sh
//! cargo run --release -p sparkles-reasoner --example rdfs_on_read -- 1000000
//! ```

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::rdfs::{RdfsOnRead, schema_of_graph};
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{INFERRED_GRAPH, Profile, ReasonOptions, materialize};
use std::sync::Arc;
use std::time::Instant;

#[path = "common/generate.rs"]
mod data;

const EX: &str = "http://bench.example.org/";

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(100_000);
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_bytes(
            data::generate(n).into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
    println!("{} triples", store.snapshot().len());
    let t = Instant::now();
    let schema = schema_of_graph(&store.snapshot(), None).unwrap();
    println!(
        "schema read in {:.1} ms: {:?} (classes, properties, domains, ranges with a declaration)",
        t.elapsed().as_secs_f64() * 1000.0,
        schema.counts()
    );
    let rdfs = Arc::new(RdfsOnRead::fixed(schema));
    let t = Instant::now();
    let r = materialize(&store, &Profile::RdfsSimple, &ReasonOptions::default()).unwrap();
    println!(
        "rdfs-simple materialized in {} ms ({} triples; wall {} ms)",
        r.millis,
        r.inferred,
        t.elapsed().as_millis()
    );
    let queries = [
        ("type, leaf class", format!("?x a <{EX}C400>")),
        ("type, inner class", format!("?x a <{EX}C1>")),
        ("type, root class", format!("?x a <{EX}C0>")),
        ("superproperty", format!("?x <{EX}p0> ?y")),
        ("constant subject", format!("<{EX}i42> ?p ?o")),
        ("constant object", format!("?s ?p <{EX}i42>")),
        ("every type", "?x a ?t".to_string()),
        (
            "join",
            format!("?x a <{EX}C1> . ?x <{EX}p0> ?y . ?y a <{EX}C2>"),
        ),
    ];
    let modes: [(&str, QueryOptions); 3] = [
        ("stored", QueryOptions::default()),
        (
            "on read",
            QueryOptions {
                rdfs: Some(rdfs.clone()),
                ..Default::default()
            },
        ),
        (
            "materialized",
            QueryOptions {
                default_graph_extra: vec![INFERRED_GRAPH.to_string()],
                ..Default::default()
            },
        ),
    ];
    println!(
        "{:<18} {:>22} {:>22} {:>22}",
        "query", "stored", "on read", "materialized"
    );
    for (name, pattern) in &queries {
        let q = format!("SELECT (COUNT(*) AS ?n) {{ {pattern} }}");
        let mut row = format!("{name:<18}");
        for (_, o) in &modes {
            let o = QueryOptions {
                no_cache: true,
                ..o.clone()
            };
            let mut times = Vec::new();
            let mut count = String::new();
            for _ in 0..5 {
                let t = Instant::now();
                let r = query(store.snapshot(), &q, &o).unwrap();
                times.push(t.elapsed().as_secs_f64() * 1000.0);
                count = r.rows()[0][0]
                    .as_ref()
                    .map(|t| match t {
                        oxrdf::Term::Literal(l) => l.value().to_string(),
                        t => t.to_string(),
                    })
                    .unwrap_or_default();
            }
            times.sort_by(f64::total_cmp);
            row.push_str(&format!(" {:>10} {:>9.1} ms", count, times[2]));
        }
        println!("{row}");
    }
}
