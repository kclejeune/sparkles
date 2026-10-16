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

/// The tests change the process-wide budget, so they run one at a time.
static BUDGET: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn config() -> VectorIndexConfig {
    VectorIndexConfig {
        hnsw: Some(HnswConfig {
            m: 8,
            ef_construction: 64,
            ef_search: 64,
        }),
        exact_threshold: 0,
        ..VectorIndexConfig::new(EMB, DIM)
    }
}

/// A store whose base holds `vs` as `<urn:n0>`, `<urn:n1>` and so on, with an index
/// built over them, and the bytes the index takes.
fn indexed_store(vs: &[Vec<f32>]) -> (Store, u64) {
    let s = Store::in_memory(StoreOptions::default());
    s.sequential_vector_builds(true);
    s.load(&[Source::from_bytes(
        rows(vs, 0).into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    s.create_vector_index("emb", config()).unwrap();
    s.wait_vector_index("emb").unwrap();
    let st = s.vector_index("emb").unwrap();
    let used = st.memory.segment_bytes + st.memory.hnsw_bytes;
    (s, used)
}

/// Catch-up stops at the budget: it inserts the rows that fit, the rows it leaves stay
/// pending, the status says why, and searches still find them by scoring them exactly.
/// Raising the budget lets catch-up go on, and the status no longer reports the stop.
#[test]
fn the_live_layer_stops_at_the_memory_budget() {
    let _held = BUDGET.lock().unwrap_or_else(|e| e.into_inner());
    let initial = vector::budget();
    let all = clustered(2_500, 41);
    let (base, extra) = all.split_at(2_000);
    let (s, used) = indexed_store(base);
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
    assert!(
        st.overlay.indexed > 0,
        "a batch is admitted in part: {:?}",
        st.overlay
    );
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
    // more room: the rows left pending join the graph
    vector::set_budget(initial);
    assert!(s.wait_vector_catch_up(std::time::Duration::from_secs(60)));
    let st = s.vector_index("emb").unwrap();
    assert_eq!(
        (st.overlay.indexed, st.overlay.pending),
        (500, 0),
        "{:?}",
        st.overlay
    );
    assert!(!st.overlay.over_budget);
}

/// A compaction releases the live layer that reached the budget, and catch-up then
/// inserts the rows that later commits add.
#[test]
fn catch_up_resumes_after_a_compaction() {
    let _held = BUDGET.lock().unwrap_or_else(|e| e.into_inner());
    let initial = vector::budget();
    let all = clustered(2_270, 53);
    let (base, rest) = all.split_at(2_000);
    let (replaced, later) = rest.split_at(250);
    let (s, used) = indexed_store(base);
    vector::set_budget(used + 30 * 1024);
    // 250 rows replaced, which keeps the base's size and needs more than the room left
    let mut body = String::new();
    for (i, v) in base.iter().enumerate().take(250) {
        body += &format!(
            "DELETE DATA {{ <urn:n{i}> <{EMB}> \"{}\"^^<urn:x-sparkles:vector> }} ;\n",
            vector::canonical(v)
        );
    }
    body += &format!("INSERT DATA {{ {} }}", rows(replaced, 20_000));
    update(&s, &body, &QueryOptions::default()).unwrap();
    assert!(s.wait_vector_catch_up(std::time::Duration::from_secs(60)));
    let st = s.vector_index("emb").unwrap();
    assert!(
        st.overlay.pending > 0 && st.overlay.over_budget,
        "{:?}",
        st.overlay
    );
    s.compact().unwrap();
    let st = s.wait_vector_index("emb").unwrap();
    assert_eq!((st.state.as_str(), st.rows), ("ready", 2_000), "{st:?}");
    assert_eq!(st.overlay.inserts, 0);
    update(
        &s,
        &format!("INSERT DATA {{ {} }}", rows(later, 30_000)),
        &QueryOptions::default(),
    )
    .unwrap();
    assert!(s.wait_vector_catch_up(std::time::Duration::from_secs(60)));
    let st = s.vector_index("emb").unwrap();
    vector::set_budget(initial);
    assert_eq!(
        (st.overlay.indexed, st.overlay.pending),
        (20, 0),
        "{:?}",
        st.overlay
    );
    assert!(!st.overlay.over_budget);
}
