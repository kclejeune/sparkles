//! The live layer of a vector index counts against the process-wide vector memory budget.
//! This file runs in its own process because the budget is a static.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update;
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::vector::{self, HnswConfig, SearchMode, VectorIndexConfig};

const EMB: &str = "urn:emb";
const DIM: usize = 32;

fn splitmix(z: &mut u64) -> f64 {
    *z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut x = *z;
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    ((x ^ (x >> 31)) >> 11) as f64 / (1u64 << 53) as f64
}

/// Clustered vectors: `n` points around 40 centres, fixed seed.
fn clustered(n: usize, seed: u64) -> Vec<Vec<f32>> {
    let mut z = seed;
    let centres: Vec<Vec<f32>> = (0..40)
        .map(|_| {
            (0..DIM)
                .map(|_| (splitmix(&mut z) * 2.0 - 1.0) as f32)
                .collect()
        })
        .collect();
    (0..n)
        .map(|_| {
            let c = &centres[(splitmix(&mut z) * 40.0) as usize];
            c.iter()
                .map(|x| x + ((splitmix(&mut z) - 0.5) * 0.6) as f32)
                .collect()
        })
        .collect()
}

fn rows(vs: &[Vec<f32>], first: usize) -> String {
    let mut s = String::new();
    for (i, v) in vs.iter().enumerate() {
        s += &format!(
            "<urn:n{}> <{EMB}> \"{}\"^^<urn:x-sparkles:vector> .\n",
            first + i,
            vector::canonical(v)
        );
    }
    s
}

/// (subject id, score) of the 10 best rows on the current snapshot, with the info.
fn top10(s: &Store, q: &[f32], mode: SearchMode) -> (Vec<(u64, f32)>, vector::SearchInfo) {
    let snap = s.snapshot();
    let pred = snap.lookup_iri(EMB).unwrap().0;
    let all = |_: u64| true;
    let (hits, info) = vector::search(
        &snap,
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
    (hits.iter().map(|h| (h.s, h.score)).collect(), info)
}

/// Catch-up stops at the budget: the rows it leaves stay pending, the status says why,
/// and searches still find them by scoring them exactly.
#[test]
fn the_live_layer_stops_at_the_memory_budget() {
    let all = clustered(2_500, 41);
    let (base, extra) = all.split_at(2_000);
    let s = Store::in_memory(StoreOptions::default());
    s.sequential_vector_builds(true);
    s.load(&[Source::from_bytes(
        rows(base, 0).into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    s.create_vector_index(
        "emb",
        VectorIndexConfig {
            hnsw: Some(HnswConfig {
                m: 8,
                ef_construction: 64,
                ef_search: 64,
            }),
            exact_threshold: 0,
            ..VectorIndexConfig::new(EMB, DIM)
        },
    )
    .unwrap();
    s.wait_vector_index("emb").unwrap();
    let st = s.vector_index("emb").unwrap();
    let used = st.memory.segment_bytes + st.memory.hnsw_bytes;
    // room for a few dozen inserted rows, less than one batch of the 500
    vector::set_budget(used + 50 * 256);
    update(
        &s,
        &format!("INSERT DATA {{ {} }}", rows(extra, 10_000)),
        &QueryOptions::default(),
    )
    .unwrap();
    assert!(s.wait_vector_catch_up(std::time::Duration::from_secs(60)));
    let st = s.vector_index("emb").unwrap();
    assert_eq!(st.overlay.inserts, 500);
    assert!(st.overlay.pending > 0, "{:?}", st.overlay);
    assert!(st.overlay.over_budget, "{:?}", st.overlay);
    assert_eq!(st.overlay.indexed + st.overlay.pending, 500);
    for q in [&extra[3], &extra[250], &base[17]] {
        let (exact, _) = top10(
            &s,
            q,
            SearchMode {
                exact: true,
                ef: None,
            },
        );
        let (got, info) = top10(&s, q, SearchMode::default());
        assert_eq!(info.method, "hnsw");
        assert_eq!(
            got.iter().map(|h| h.0).collect::<Vec<_>>(),
            exact.iter().map(|h| h.0).collect::<Vec<_>>()
        );
    }
}
