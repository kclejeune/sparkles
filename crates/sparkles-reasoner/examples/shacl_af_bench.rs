//! The cost of the `shacl-af` profile per round (roadmap items 838 and 958).
//!
//! `cargo run --release -p sparkles-reasoner --example shacl_af_bench -- [N...]` makes N
//! rectangles with a width and a height, a third of them squares, and runs three rule
//! groups over them: a triple rule that computes each area with a `sh:SPARQLFunction`, a
//! SPARQL rule with a condition that types the squares, and in a later order a triple rule
//! that marks the squares whose area it reads. It prints the time, the rounds and the time
//! per round and per focus node of each size.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::shacl_af::ShaclAf;
use sparkles_reasoner::{Extras, Profile, ReasonOptions, materialize_with};
use std::fmt::Write as _;
use std::time::Instant;

const SHAPES: &str = r#"
@prefix ex: <http://example.org/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
ex:multiply a sh:SPARQLFunction ;
  sh:parameter [ sh:path ex:op1 ] ; sh:parameter [ sh:path ex:op2 ] ;
  sh:select "SELECT ($op1 * $op2 AS ?result) WHERE { }" .
ex:RectangleShape a sh:NodeShape ; sh:targetClass ex:Rectangle ;
  sh:rule [ a sh:TripleRule ; sh:order 1 ; sh:subject sh:this ; sh:predicate ex:area ;
    sh:object [ ex:multiply ( [ sh:path ex:width ] [ sh:path ex:height ] ) ] ] ;
  sh:rule [ a sh:SPARQLRule ; sh:order 1 ;
    sh:condition [ sh:property [ sh:path ex:width ; sh:equals ex:height ] ] ;
    sh:construct "CONSTRUCT { $this a <http://example.org/Square> } WHERE { }" ] .
ex:SquareShape a sh:NodeShape ; sh:targetClass ex:Square ;
  sh:rule [ a sh:TripleRule ; sh:order 2 ; sh:subject sh:this ; sh:predicate ex:squareArea ;
    sh:object [ sh:path ex:area ] ] .
"#;

fn main() {
    let sizes: Vec<usize> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let sizes = if sizes.is_empty() {
        vec![1_000, 5_000, 20_000]
    } else {
        sizes
    };
    println!("focus nodes,inferred,rounds,ms,ms per round,us per focus node and round");
    for n in sizes {
        let mut data = String::from("@prefix ex: <http://example.org/> .\n");
        for i in 0..n {
            let w = 1 + i % 7;
            let h = if i % 3 == 0 { w } else { w + 1 };
            writeln!(
                data,
                "ex:r{i} a ex:Rectangle ; ex:width {w} ; ex:height {h} ."
            )
            .unwrap();
        }
        let store = Store::in_memory(StoreOptions::default());
        store
            .load(&[
                Source::from_bytes(data.into_bytes(), RdfFormat::Turtle, None),
                Source::from_bytes(SHAPES.as_bytes().to_vec(), RdfFormat::Turtle, None),
            ])
            .unwrap();
        let t = Instant::now();
        let r = materialize_with(
            &store,
            &Profile::ShaclAf(ShaclAf::default()),
            &Extras::default(),
            &ReasonOptions::default(),
        )
        .unwrap();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        let rounds: usize = r.rule_groups.iter().map(|g| g.rounds).sum();
        println!(
            "{n},{},{rounds},{ms:.1},{:.1},{:.2}",
            r.inferred,
            ms / rounds as f64,
            ms * 1e3 / rounds as f64 / n as f64
        );
    }
}
