//! `cargo run --release --example modeprobe -- DB_DIR ROUNDS QUERY_FILE...` compares eager
//! and streaming execution of each query over an existing database directory.
//!
//! Each round runs every query once eagerly and once as a cursor, in alternating order,
//! both reading every row (`rows`) and writing TSV to a sink (`tsv`). It prints the median
//! times, their ratio (streaming over eager, under 1 means streaming is faster), the peak
//! of each mode's memory ledger and what automatic execution chose. `SPARKLES_PROBE_MODES`
//! picks `rows`, `tsv` or both (the default), and `SPARKLES_PROBE_MEMORY_MB` sets the
//! query memory budget. `SPARKLES_PROBE_STREAM_MEMORY_MB` sets another budget for the
//! cursor runs, and `SPARKLES_PROBE_SPILL_DIR` lets them spill to that directory.
use sparkles_core::sparql::results::{SolutionsFormat, write_cursor_solutions, write_solutions};
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, QueryExecution, QueryOptions, query, query_cursor,
    query_execution,
};
use sparkles_core::store::{Store, StoreOptions};
use std::time::Instant;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    if v.is_empty() {
        return f64::NAN;
    }
    v[v.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let store = Store::open(std::path::Path::new(&args[1]), StoreOptions::default()).unwrap();
    let rounds: usize = args[2].parse().unwrap();
    let modes = std::env::var("SPARKLES_PROBE_MODES").unwrap_or_else(|_| "rows,tsv".into());
    let memory = std::env::var("SPARKLES_PROBE_MEMORY_MB")
        .ok()
        .map(|m| m.parse::<u64>().unwrap() << 20);
    let options = QueryOptions {
        no_cache: true,
        max_memory_bytes: memory,
        ..Default::default()
    };
    let mut stream_options = options.clone();
    if let Ok(mb) = std::env::var("SPARKLES_PROBE_STREAM_MEMORY_MB") {
        stream_options.max_memory_bytes = Some(mb.parse::<u64>().unwrap() << 20);
    }
    if let Ok(dir) = std::env::var("SPARKLES_PROBE_SPILL_DIR") {
        let spill = sparkles_core::spill::Spill::open(&sparkles_core::spill::SpillOptions {
            dir: dir.into(),
            max_bytes: 64 << 30,
            query_bytes: 64 << 30,
            min_free_bytes: 1 << 30,
        })
        .unwrap();
        stream_options.spill = Some(spill);
    }
    let cursor = CursorOptions::default();
    // `eager` or `stream` runs only that mode, for a profiler
    let only = std::env::var("SPARKLES_PROBE_ONLY").ok();
    println!(
        "{:<22} {:>5} {:>10} {:>10} {:>6} {:>10} {:>10} {:>9} {:>6}",
        "query", "mode", "eager ms", "stream ms", "ratio", "eager MB", "stream MB", "rows", "auto"
    );
    for file in &args[3..] {
        let name = std::path::Path::new(file)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let text = std::fs::read_to_string(file).unwrap();
        let auto = match query_execution(
            store.snapshot(),
            &text,
            &options,
            &cursor,
            ExecutionMode::Auto,
        ) {
            Ok(QueryExecution::Eager(_)) => "eager",
            Ok(QueryExecution::Select(_)) => "stream",
            Ok(_) => "other",
            Err(_) => "error",
        };
        for mode in modes.split(',') {
            let (mut eager, mut stream) = (Vec::new(), Vec::new());
            let (mut eager_mem, mut stream_mem, mut rows) = (0u64, 0u64, 0usize);
            for round in 0..rounds + 1 {
                for first in [round % 2 == 0, round % 2 == 1] {
                    match only.as_deref() {
                        Some("eager") if !first => continue,
                        Some("stream") if first => continue,
                        _ => {}
                    }
                    let started = Instant::now();
                    if first {
                        let result = query(store.snapshot(), &text, &options).unwrap();
                        if mode == "tsv" {
                            write_solutions(&result, SolutionsFormat::Tsv, std::io::sink(), None)
                                .unwrap();
                        }
                        rows = result.len();
                        if round == rounds && std::env::var_os("SPARKLES_PROBE_PLAN").is_some() {
                            eprintln!(
                                "{name} eager plan: {}",
                                serde_json::to_string(&result.plan).unwrap()
                            );
                        }
                        eager_mem = eager_mem.max(result.mem_peak_bytes);
                        drop(result);
                        if round > 0 {
                            eager.push(started.elapsed().as_secs_f64() * 1000.0);
                        }
                    } else {
                        let Ok(QueryExecution::Select(mut c)) =
                            query_cursor(store.snapshot(), &text, &stream_options, &cursor)
                        else {
                            panic!("{name}: not a SELECT cursor");
                        };
                        let stats = if mode == "tsv" {
                            write_cursor_solutions(
                                &mut c,
                                SolutionsFormat::Tsv,
                                std::io::sink(),
                                None,
                            )
                            .unwrap()
                        } else {
                            while let Some(batch) = c.next_batch().unwrap() {
                                std::hint::black_box(batch.len());
                            }
                            c.stats()
                        };
                        stream_mem = stream_mem.max(stats.mem_peak_bytes);
                        if round == rounds && std::env::var_os("SPARKLES_PROBE_PLAN").is_some() {
                            eprintln!("{name} cursor plan: {}", c.plan_json().unwrap());
                        }
                        drop(c);
                        if round > 0 {
                            stream.push(started.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                }
            }
            let (e, s) = (median(&mut eager), median(&mut stream));
            println!(
                "{:<22} {:>5} {:>10.3} {:>10.3} {:>6.2} {:>10.1} {:>10.1} {:>9} {:>6}",
                name,
                mode,
                e,
                s,
                s / e,
                eager_mem as f64 / 1048576.0,
                stream_mem as f64 / 1048576.0,
                rows,
                auto
            );
        }
    }
}
