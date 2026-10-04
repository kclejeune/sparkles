//! The Rust side of the JVM bindings' performance check (P04 §5.4): the same queries the
//! Kotlin check runs, through `sparkles::Dataset` in process, timed the same way.
//!
//! `cargo run --release --example perf -- DATA.nt QUERIES.tsv ITERATIONS`, where each line
//! of QUERIES.tsv is a name, a tab and the query text. It prints a name, the median
//! milliseconds and the rows for each query.

use sparkles::Dataset;
use sparkles::sparql::QueryOptions;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (data, queries, iters) = (&args[1], &args[2], args[3].parse::<usize>()?);
    let ds = Dataset::memory();
    let t = Instant::now();
    ds.load_file(data)?;
    eprintln!("loaded {} quads in {:?}", ds.len(), t.elapsed());
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    for line in std::fs::read_to_string(queries)?.lines() {
        let Some((name, q)) = line.split_once('\t') else {
            continue;
        };
        let mut times = Vec::new();
        let mut rows = 0;
        for i in 0..iters + 3 {
            let t = Instant::now();
            let r = ds.query_with(q, &opts)?;
            // decode every cell, as a caller that reads the solutions would
            let mut n = 0;
            for row in r.rows() {
                n += row.iter().filter(|c| c.is_some()).count();
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            rows = r.table.len();
            let _ = n;
            if i >= 3 {
                times.push(ms);
            }
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("{name}\t{:.3}\t{rows}", times[times.len() / 2]);
    }
    Ok(())
}
