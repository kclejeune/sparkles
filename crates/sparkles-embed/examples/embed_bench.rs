//! Download a snapshot and measure a model (spec F12 §9, Outcome).
//!
//! ```sh
//! cargo run --release -p sparkles-embed --example embed-bench -- pull ROOT REPO REVISION
//! cargo run --release -p sparkles-embed --example embed-bench -- run DIR [f32|bf16] [THREADS]
//! ```
//!
//! `run` reports the load time, the resident memory after loading, the latency of a
//! single short query (median and 95th percentile of 50) and the throughput of 256
//! documents of about 60 words.

use std::time::{Duration, Instant};

use sparkles_embed::{Dtype, Embedder, Kind, ModelSpec, Options};
use sparkles_modelstore::{HttpClient, HttpResponse, HubSource, ModelStore};

struct Reqwest(reqwest::blocking::Client);

impl HttpClient for Reqwest {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, String> {
        let mut rb = self.0.get(url);
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        let r = rb.send().map_err(|e| e.to_string())?;
        Ok(HttpResponse {
            status: r.status().as_u16(),
            body: Box::new(r),
        })
    }
}

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| {
            s.split_whitespace()
                .nth(1)
                .and_then(|p| p.parse::<f64>().ok())
        })
        .map_or(0.0, |p| p * 4096.0 / 1e6)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("pull") => {
            let client = Reqwest(
                reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(3600))
                    .build()
                    .unwrap(),
            );
            let hub = HubSource::default();
            let plan = hub
                .plan(
                    &client,
                    &args[2],
                    &args[3],
                    false,
                    &sparkles_embed::hub_select,
                )
                .unwrap();
            println!("{} files, {} bytes", plan.files.len(), plan.bytes());
            let t = Instant::now();
            let s = ModelStore::new(&args[1])
                .fetch(&client, &plan, &mut |p| {
                    eprintln!("{}/{} files", p.files_done, p.files_total)
                })
                .unwrap();
            println!("{} in {:.1} s", s.dir.display(), t.elapsed().as_secs_f64());
        }
        Some("run") => {
            let mut spec = ModelSpec::new(&args[1]);
            spec.dtype = match args.get(2).map(String::as_str) {
                Some("bf16") => Dtype::Bf16,
                _ => Dtype::F32,
            };
            let threads = args.get(3).and_then(|t| t.parse().ok()).unwrap_or(2);
            let base = rss_mb();
            let e = Embedder::new(
                spec,
                Options {
                    threads,
                    nice: 0,
                    ..Options::default()
                },
            )
            .unwrap();
            let t = Instant::now();
            e.embed(&["warm up"], Kind::Query).unwrap();
            let s = e.status();
            println!(
                "{:?} {:?} threads={threads} load={} ms first={:.0} ms weights={:.0} MB rss+={:.0} MB dim={}",
                e.info().arch,
                e.info().dtype,
                s.last_load_ms.unwrap_or(0),
                t.elapsed().as_secs_f64() * 1e3,
                s.weight_bytes.unwrap_or(0) as f64 / 1e6,
                rss_mb() - base,
                e.info().dimension
            );
            let mut lat: Vec<f64> = (0..50)
                .map(|i| {
                    let q = format!("which rivers flow through southern France {i}");
                    let t = Instant::now();
                    e.embed(&[&q], Kind::Query).unwrap();
                    t.elapsed().as_secs_f64() * 1e3
                })
                .collect();
            lat.sort_by(f64::total_cmp);
            println!("query latency p50={:.1} ms p95={:.1} ms", lat[25], lat[47]);
            let doc = "The Rhone rises in the Swiss Alps, flows through Lake Geneva and \
                       then south through France past Lyon and Avignon before it reaches \
                       the Mediterranean Sea west of Marseille, where its delta forms the \
                       wetlands of the Camargue, known for flamingos, white horses and \
                       rice fields that depend on the river's water.";
            let docs: Vec<String> = (0..256).map(|i| format!("{i}. {doc}")).collect();
            let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
            let t = Instant::now();
            e.embed(&refs, Kind::Document).unwrap();
            let secs = t.elapsed().as_secs_f64();
            println!(
                "256 documents of ~60 words: {:.2} s, {:.1} texts/s, rss+={:.0} MB",
                secs,
                256.0 / secs,
                rss_mb() - base
            );
            e.unload();
            std::thread::sleep(Duration::from_millis(500));
            println!("after unload rss+={:.0} MB", rss_mb() - base);
        }
        _ => eprintln!("usage: embed-bench pull ROOT REPO REVISION | run DIR [f32|bf16] [THREADS]"),
    }
}
