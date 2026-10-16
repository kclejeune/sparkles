//! `cargo run --release --example vector_build -- N DIM [INSERTS]`: time a configured
//! vector index over N clustered vectors of dimension DIM in an in-memory store. It
//! reports the build, recall@10 against the exact search for held-out queries, and then
//! the compaction that folds in INSERTS new vectors (default N / 100).
//!
//! The vectors follow `scripts/bench-vector.sh`: 1000 centres drawn from N(0, 1) and
//! Gaussian noise of 0.6, cosine, M = 16, efConstruction = 128. Env: EFS (default 128),
//! QUERIES (default 200), SEED (default 42).

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::vector::{self, HnswConfig, SearchMode, VectorIndexConfig};
use std::time::Instant;

const EMB: &str = "urn:x-bench:emb";

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (((x ^ (x >> 31)) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f32 {
        let (u, v) = (self.next(), self.next());
        ((-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()) as f32
    }
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn sources(rows: &[Vec<f32>], first: usize, per: usize) -> Vec<Source> {
    rows.chunks(per)
        .enumerate()
        .map(|(c, chunk)| {
            let mut text = String::new();
            for (i, v) in chunk.iter().enumerate() {
                text += &format!(
                    "<urn:x-bench:v{}> <{EMB}> \"{}\"^^<{}> .\n",
                    first + c * per + i,
                    vector::canonical_b64(v),
                    vector::DATATYPE_B64
                );
            }
            Source::from_bytes(text.into_bytes(), RdfFormat::NTriples, None)
        })
        .collect()
}

fn top(
    snap: &sparkles_core::store::Snapshot,
    q: &[f32],
    mode: SearchMode,
) -> (Vec<u64>, vector::SearchInfo) {
    let pred = snap.lookup_iri(EMB).unwrap().0;
    let all = |_: u64| true;
    let (hits, info) = vector::search(
        snap,
        &vector::Search {
            pred,
            query: q,
            k: 10,
            metric: vector::Metric::Cosine,
            graph: &all,
            dedup: false,
            distinct_subject: false,
            subjects: None,
            mode,
        },
        &|| Ok(()),
    )
    .unwrap();
    (hits.iter().map(|h| h.s).collect(), info)
}

fn recall(s: &Store, queries: &[Vec<f32>], ef: usize) -> (f64, f64, &'static str) {
    let snap = s.snapshot();
    let (mut hits, mut total, mut ms) = (0usize, 0usize, 0.0);
    let mut method = "";
    for q in queries {
        let (exact, _) = top(
            &snap,
            q,
            SearchMode {
                exact: true,
                ef: None,
            },
        );
        let t = Instant::now();
        let (ann, info) = top(
            &snap,
            q,
            SearchMode {
                exact: false,
                ef: Some(ef),
            },
        );
        ms += t.elapsed().as_secs_f64() * 1e3;
        method = info.method;
        hits += ann.iter().filter(|a| exact.contains(a)).count();
        total += exact.len();
    }
    (
        hits as f64 / total as f64,
        ms / queries.len() as f64,
        method,
    )
}

fn main() {
    let args: Vec<usize> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("N DIM [INSERTS]"))
        .collect();
    let (n, dim) = (args[0], args[1]);
    let inserts = args.get(2).copied().unwrap_or(n / 100);
    let nq = env("QUERIES", 200);
    let ef = env("EFS", 128);
    let mut rng = Rng(env("SEED", 42) as u64);
    let centres: Vec<Vec<f32>> = (0..1000)
        .map(|_| (0..dim).map(|_| rng.normal()).collect())
        .collect();
    let mut points = |count: usize| -> Vec<Vec<f32>> {
        (0..count)
            .map(|_| {
                let c = &centres[(rng.next() * 1000.0) as usize];
                c.iter().map(|x| x + 0.6 * rng.normal()).collect()
            })
            .collect()
    };
    let base = points(n);
    let extra = points(inserts);
    let queries = points(nq);

    let s = Store::in_memory(StoreOptions::default());
    let t = Instant::now();
    s.load(&sources(&base, 0, 50_000)).unwrap();
    s.compact().unwrap();
    println!("load {n} x {dim}: {:.1} s", t.elapsed().as_secs_f64());

    let cfg = VectorIndexConfig {
        hnsw: Some(HnswConfig {
            m: 16,
            ef_construction: 128,
            ef_search: ef,
        }),
        ..VectorIndexConfig::new(EMB, dim)
    };
    let t = Instant::now();
    s.create_vector_index("emb", cfg).unwrap();
    let st = s.wait_vector_index("emb").unwrap();
    println!(
        "build: {:.2} s ({} rows, {} nodes, state {})",
        t.elapsed().as_secs_f64(),
        st.rows,
        st.hnsw.as_ref().map_or(0, |h| h.nodes),
        st.state
    );
    let (r, ms, m) = recall(&s, &queries, ef);
    println!("recall@10 at ef={ef}: {r:.4}, {ms:.3} ms per query ({m})");

    let t = Instant::now();
    for src in sources(&extra, n, 1000) {
        s.load(&[src]).unwrap();
    }
    let committed = t.elapsed().as_secs_f64();
    let caught = s.wait_vector_catch_up(std::time::Duration::from_secs(3600));
    let st = s.vector_index("emb").unwrap();
    println!(
        "insert {inserts} in commits of 1000: {committed:.2} s; in the graph {:.2} s after the \
         first commit (caught up {caught}, indexed {}, pending {})",
        t.elapsed().as_secs_f64(),
        st.overlay.indexed,
        st.overlay.pending
    );
    let (r, ms, m) = recall(&s, &queries, ef);
    println!("recall@10 at ef={ef} with the inserts: {r:.4}, {ms:.3} ms per query ({m})");
    // deletes as many base rows as half the inserts, spread over the base
    let gone = inserts / 2;
    for chunk in (0..gone).collect::<Vec<_>>().chunks(500) {
        let mut u = String::new();
        for &j in chunk {
            let i = j * (n / gone.max(1));
            u += &format!(
                "DELETE DATA {{ <urn:x-bench:v{i}> <{EMB}> \"{}\"^^<{}> }} ;\n",
                vector::canonical_b64(&base[i]),
                vector::DATATYPE_B64
            );
        }
        sparkles_core::sparql::update::update(&s, u.trim_end_matches(";\n"), &Default::default())
            .unwrap();
    }
    let t = Instant::now();
    s.compact().unwrap();
    let compact = t.elapsed().as_secs_f64();
    let st = s.wait_vector_index("emb").unwrap();
    println!(
        "compaction after {gone} deletes: {compact:.2} s, index ready {:.2} s after the start \
         ({} rows, state {}, carried {})",
        t.elapsed().as_secs_f64(),
        st.rows,
        st.state,
        st.last_build.as_ref().is_some_and(|b| b.carried)
    );
    let (r, ms, m) = recall(&s, &queries, ef);
    println!("recall@10 at ef={ef} after the compaction: {r:.4}, {ms:.3} ms per query ({m})");
    let t = Instant::now();
    s.rebuild_vector_index("emb").unwrap();
    s.wait_vector_index("emb").unwrap();
    let (r, ms, m) = recall(&s, &queries, ef);
    println!(
        "full rebuild of the same generation: {:.2} s, recall@10 at ef={ef}: {r:.4}, {ms:.3} ms \
         per query ({m})",
        t.elapsed().as_secs_f64()
    );
}
