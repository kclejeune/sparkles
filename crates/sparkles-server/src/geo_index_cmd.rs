//! `sparkles geo-index`: build, rebuild or inspect the spatial index of a database
//! directory through the dataset's spatial index handle.

use anyhow::{Context, Result};
use serde_json::Value as J;
use sparkles::store::StoreOptions;

/// The options of `sparkles geo-index`.
pub struct IndexArgs {
    pub predicate: Vec<String>,
    pub feature_link: Vec<String>,
    pub exclude_graph: Vec<String>,
    pub distance: Option<String>,
    pub wgs84: bool,
    pub rebuild: bool,
    pub status: bool,
    pub disable: bool,
}

/// Enable the index (with the defaults, if needed), build it and print the status.
/// `--status` prints it as JSON and changes nothing.
pub fn run(loc: &std::path::Path, opts: StoreOptions, a: IndexArgs) -> Result<()> {
    if !cfg!(feature = "geo") {
        eprintln!("built without GeoSPARQL (cargo feature \"geo\")");
        std::process::exit(2)
    }
    let geo = sparkles::Dataset::open_with(loc, opts)?.indexes().geo();
    if a.disable {
        geo.disable()?;
        eprintln!("spatial index disabled");
        return Ok(());
    }
    // the index is built in memory when the store opens: report it once built
    let current = geo.wait();
    if a.status {
        println!(
            "{}",
            match current {
                Some(s) => serde_json::to_string_pretty(&s)?,
                None => r#"{ "enabled": false }"#.to_string(),
            }
        );
        return Ok(());
    }
    let t = std::time::Instant::now();
    let configured = !a.predicate.is_empty()
        || !a.feature_link.is_empty()
        || !a.exclude_graph.is_empty()
        || a.distance.is_some()
        || a.wgs84;
    let s = match current {
        Some(_) if !configured && a.rebuild => geo.rebuild()?,
        Some(s) if !configured => s,
        current => {
            let mut cfg = current.map(|s| s.config).unwrap_or_default();
            if !a.predicate.is_empty() {
                cfg.predicates = a.predicate;
            }
            if !a.feature_link.is_empty() {
                cfg.feature_links = a.feature_link;
            }
            if !a.exclude_graph.is_empty() {
                cfg.graphs.exclude = a.exclude_graph;
            }
            if a.wgs84 {
                cfg.wgs84 = true;
            }
            if let Some(d) = a.distance {
                cfg.distance = serde_json::from_value(J::String(d.clone()))
                    .with_context(|| format!("--distance {d}: geodesic or haversine"))?;
            }
            geo.enable(cfg)?
        }
    };
    eprintln!(
        "spatial index: {} rows ({} base, {} overlay, {} tail), {} literals, {} in {:.0} ms",
        s.rows.base + s.rows.overlay + s.rows.tail,
        s.rows.base,
        s.rows.overlay,
        s.rows.tail,
        s.literals,
        s.state,
        t.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}
