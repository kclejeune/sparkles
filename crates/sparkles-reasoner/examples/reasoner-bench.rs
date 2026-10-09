//! Materialization benchmark over a generated LUBM-like dataset.
//!
//! ```sh
//! cargo run --release -p sparkles-reasoner --example reasoner-bench -- 100000 rdfs
//! ```

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{Profile, ReasonOptions, infer, materialize};
use std::time::Instant;

#[path = "common/generate.rs"]
mod data;
use data::generate;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(100_000);
    let profiles: Vec<Profile> = match args.get(2) {
        Some(p) => vec![p.parse().expect("profile")],
        None => vec![Profile::RdfsSimple, Profile::Rdfs, Profile::OwlRl],
    };
    let data = generate(n);
    for profile in profiles {
        let store = Store::in_memory(StoreOptions::default());
        let t = Instant::now();
        store
            .load(&[Source::from_bytes(
                data.clone().into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let loaded = store.snapshot().len();
        let load_ms = t.elapsed().as_millis();
        let t = Instant::now();
        let (triples, _) = infer(store.snapshot(), &profile, &ReasonOptions::default()).unwrap();
        println!(
            "{:<12} reasoning only (infer, no write): {} triples in {} ms",
            profile.name(),
            triples.len(),
            t.elapsed().as_millis()
        );
        let r = materialize(&store, &profile, &ReasonOptions::default()).unwrap();
        println!(
            "{:<12} base {loaded:>9} triples (load {load_ms} ms) -> inferred {:>9} in {:>6} ms ({} iterations, {} rules)",
            r.profile, r.inferred, r.millis, r.iterations, r.rules
        );
        let t = Instant::now();
        let r2 = materialize(&store, &profile, &ReasonOptions::default()).unwrap();
        println!(
            "{:<12} re-materialize (no changes): {} ms",
            "",
            t.elapsed().as_millis()
        );
        assert_eq!(r.inferred, r2.inferred);
    }
}
