//! Validation benchmark on a `scripts/gen-data.py` dataset, with `bench.shex`.
//!
//! ```sh
//! python3 scripts/gen-data.py 10000 > data.nt
//! cargo run --release -p sparkles-shex --example bench -- data.nt [schema.shex|-] [iterations] [map.smap]
//! ```
//!
//! Prints the load, parse and compile, and shape-map expansion times; the validation
//! time, sequential and parallel (median of the iterations); the results by the kind of
//! their first failure; and the latency of single-node validation against the
//! non-recursive `ex:Org` shape.

use sparkles_core::io::Source;
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::validation::DataGraph;
use sparkles_shex::{
    NoImports, ResultMap, Schema, ShapeLabel, ShapeMap, ShexFailure, Status, ValidateOptions,
    compile, validate, validate_node,
};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

const SCHEMA: &str = include_str!("bench.shex");

const MAP: &str = include_str!("bench.smap");

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// The kind of a failure, as in the JSON report.
fn kind(f: &ShexFailure) -> &'static str {
    match f {
        ShexFailure::NodeKind { .. } => "nodeKind",
        ShexFailure::Datatype { .. } => "datatype",
        ShexFailure::Facet { .. } => "facet",
        ShexFailure::ValueSet { .. } => "valueSet",
        ShexFailure::Cardinality { .. } => "cardinality",
        ShexFailure::Closed { .. } => "closed",
        ShexFailure::Extra { .. } => "extra",
        ShexFailure::NoMatch { .. } => "noMatch",
        ShexFailure::Reference { .. } => "reference",
        ShexFailure::Not { .. } => "not",
        ShexFailure::SemAct { .. } => "semAct",
        ShexFailure::External { .. } => "external",
    }
}

fn by_reason(r: &ResultMap) -> BTreeMap<&'static str, usize> {
    let mut by = BTreeMap::new();
    for x in r
        .results
        .iter()
        .filter(|x| x.status == Status::Nonconformant)
    {
        let k = x.failures.first().map_or("unexplained", kind);
        *by.entry(k).or_default() += 1;
    }
    by
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let Some(data) = args.get(1) else {
        eprintln!("usage: bench DATA.nt [SCHEMA.shex|-] [ITERATIONS] [MAP.smap]");
        std::process::exit(2);
    };
    let schema_text = match args.get(2).filter(|s| *s != "-") {
        Some(p) => std::fs::read_to_string(p)?,
        None => SCHEMA.to_string(),
    };
    let iters: usize = args.get(3).map_or(Ok(5), |s| s.parse())?;
    let map_text = match args.get(4) {
        Some(p) => std::fs::read_to_string(p)?,
        None => MAP.to_string(),
    };

    let t = Instant::now();
    let store = Store::in_memory(StoreOptions::default());
    let n = store.load(&[Source::from_path(Path::new(data), None)?])?;
    println!("load: {n} triples in {:.1} ms", ms(t));
    let snap = store.snapshot();

    let t = Instant::now();
    let schema = Schema::parse_shexc(&schema_text, None)?;
    let parsed = ms(t);
    let schema = compile(&schema, &NoImports)?;
    println!(
        "schema: parsed in {parsed:.2} ms, compiled in {:.2} ms",
        ms(t) - parsed
    );

    let map = ShapeMap::parse(&map_text, schema.prefixes(), schema.base())?;
    let t = Instant::now();
    let data_graph = DataGraph::new(snap.clone(), None, &[], &[])?;
    let (fixed, _) =
        sparkles_shex::shapemap::expand(&map, &data_graph, &schema, &Default::default(), None)?;
    println!(
        "shape map: {} associations expanded in {:.1} ms",
        fixed.len(),
        ms(t)
    );

    for parallel in [false, true] {
        let opts = ValidateOptions {
            parallel,
            ..Default::default()
        };
        let mut times = Vec::new();
        let mut last = None;
        for _ in 0..iters.max(1) {
            let t = Instant::now();
            let r = validate(&snap, &schema, &map, &opts)?;
            times.push(ms(t));
            last = Some(r);
        }
        times.sort_by(f64::total_cmp);
        let r = last.unwrap_or_default();
        println!(
            "validate ({}): median {:.1} ms, min {:.1} ms over {} runs; conforms={} \
             conformant={} nonconformant={} {:?}",
            if parallel { "parallel" } else { "sequential" },
            times[times.len() / 2],
            times[0],
            times.len(),
            r.conforms,
            r.conformant,
            r.nonconformant,
            by_reason(&r)
        );
        println!(
            "  typing: {} pairs, {} evaluations, refinement waves per stratum {:?}",
            r.stats.pairs, r.stats.evaluations, r.stats.waves
        );
    }

    // single nodes against the non-recursive ex:Org shape
    let org = ShapeLabel::Iri("http://example.org/Org".into());
    let orgs: Vec<_> = fixed
        .iter()
        .filter(|e| e.shape == org)
        .take(1000)
        .map(|e| e.node.clone())
        .collect();
    if !orgs.is_empty() {
        let opts = ValidateOptions {
            parallel: false,
            ..Default::default()
        };
        let mut times = Vec::with_capacity(orgs.len());
        for node in &orgs {
            let t = Instant::now();
            validate_node(&snap, &schema, node, &org, &opts)?;
            times.push(ms(t));
        }
        times.sort_by(f64::total_cmp);
        println!(
            "validate_node (ex:Org): p50 {:.3} ms, p99 {:.3} ms over {} nodes",
            times[times.len() / 2],
            times[times.len() * 99 / 100],
            times.len()
        );
    }
    Ok(())
}
