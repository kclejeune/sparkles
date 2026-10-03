//! Configured vector indexes: the HNSW graph against the exact oracle, the delta overlay,
//! point-in-time reads, files, background builds, and the search options that read the
//! rest of the group.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{QueryOptions, query};
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

fn nt(rows: &[(usize, &Vec<f32>)]) -> String {
    let mut s = String::new();
    for (i, v) in rows {
        s += &format!(
            "<urn:n{i}> <{EMB}> \"{}\"^^<urn:x-sparkles:vector> .\n",
            vector::canonical(v)
        );
    }
    s
}

fn load(s: &Store, text: String) {
    s.load(&[Source::from_bytes(
        text.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
}

/// A store whose base holds `vs`.
fn store_with(s: Store, vs: &[Vec<f32>]) -> Store {
    load(&s, nt(&vs.iter().enumerate().collect::<Vec<_>>()));
    s.compact().unwrap();
    s
}

fn config(threshold: usize) -> VectorIndexConfig {
    VectorIndexConfig {
        hnsw: Some(HnswConfig {
            m: 8,
            ef_construction: 64,
            ef_search: 64,
        }),
        exact_threshold: threshold,
        ..VectorIndexConfig::new(EMB, DIM)
    }
}

/// (subject id, score) of a search on the current snapshot, with its info.
fn search(
    snap: &sparkles_core::store::Snapshot,
    q: &[f32],
    k: usize,
    mode: SearchMode,
) -> (Vec<(u64, f32)>, vector::SearchInfo) {
    let pred = snap.lookup_iri(EMB).unwrap().0;
    let all = |_: u64| true;
    let (hits, info) = vector::search(
        snap,
        &vector::Search {
            pred,
            query: q,
            k,
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

const EXACT: SearchMode = SearchMode {
    exact: true,
    ef: None,
};

fn recall(got: &[(u64, f32)], exact: &[(u64, f32)]) -> f64 {
    got.iter()
        .filter(|g| exact.iter().any(|e| e.0 == g.0))
        .count() as f64
        / exact.len() as f64
}

#[test]
fn recall_against_the_exact_oracle() {
    // queries from the same distribution as the data, not loaded
    let all = clustered(20_200, 7);
    let (vs, queries) = all.split_at(20_000);
    let s = store_with(Store::in_memory(StoreOptions::default()), vs);
    assert!(s.create_vector_index("emb", config(0)).unwrap());
    let st = s.wait_vector_index("emb").unwrap();
    assert_eq!(st.state, "ready", "{st:?}");
    assert_eq!(st.rows, 20_000);
    assert_eq!(st.hnsw.unwrap().nodes, 20_000);
    let snap = s.snapshot();
    let (mut sum, mut sum_low) = (0.0, 0.0);
    for q in queries {
        let (exact, info) = search(&snap, q, 10, EXACT);
        assert_eq!(info.method, "exact");
        let (ann, info) = search(&snap, q, 10, SearchMode::default());
        assert_eq!((info.method, info.ef), ("hnsw", 64));
        assert!(info.scored < 5_000, "{}", info.scored);
        // a row's score does not depend on the path
        for a in &ann {
            if let Some(e) = exact.iter().find(|e| e.0 == a.0) {
                assert_eq!(a.1.to_bits(), e.1.to_bits());
            }
        }
        sum += recall(&ann, &exact);
        let (low, _) = search(
            &snap,
            q,
            10,
            SearchMode {
                exact: false,
                ef: Some(10),
            },
        );
        sum_low += recall(&low, &exact);
    }
    let (r, r_low) = (sum / 200.0, sum_low / 200.0);
    assert!(r >= 0.95, "recall@10 at ef=64: {r}");
    assert!(r >= r_low, "ef=64 {r} < ef=10 {r_low}");
    // the exact path of an index equals the implicit partition's (Phase 1)
    s.drop_vector_index("emb").unwrap();
    let plain = s.snapshot();
    for q in queries.iter().take(20) {
        let (a, info) = search(&plain, q, 10, SearchMode::default());
        assert!(info.index.is_none());
        let (b, _) = search(&snap, q, 10, EXACT);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!((x.0, x.1.to_bits()), (y.0, y.1.to_bits()));
        }
    }
}

#[test]
fn the_delta_is_overlaid_exactly() {
    let vs = clustered(5_000, 3);
    let s = store_with(Store::in_memory(StoreOptions::default()), &vs);
    // the graph searches below are compared with exact ones, so the graph must not depend
    // on thread timing
    s.sequential_vector_builds(true);
    s.create_vector_index("emb", config(0)).unwrap();
    s.wait_vector_index("emb").unwrap();
    let q = vs[42].clone();
    let n42 = s.snapshot().lookup_iri("urn:n42").unwrap().0;
    // a wide search, so the stored vector is found for itself
    let wide = SearchMode {
        exact: false,
        ef: Some(200),
    };
    let (hits, info) = search(&s.snapshot(), &q, 5, wide);
    assert_eq!(info.method, "hnsw");
    assert_eq!(hits[0].0, n42);
    // a new row, the only one in its direction, comes first for itself
    let near: Vec<f32> = (0..DIM)
        .map(|i| if i % 2 == 0 { 3.0 } else { -3.0 })
        .collect();
    load(&s, nt(&[(900_001, &near)]));
    let n_new = s.snapshot().lookup_iri("urn:n900001").unwrap().0;
    let before_delete = s.snapshot();
    let (hits, info) = search(&s.snapshot(), &near, 5, SearchMode::default());
    assert_eq!((info.method, info.inserted), ("hnsw", 1));
    assert_eq!(hits[0].0, n_new);
    // a deleted base row is gone, and comes back when inserted again
    let del = format!(
        "DELETE DATA {{ <urn:n42> <{EMB}> \"{}\"^^<urn:x-sparkles:vector> }}",
        vector::canonical(&q)
    );
    update(&s, &del, &QueryOptions::default()).unwrap();
    let (hits, info) = search(&s.snapshot(), &q, 5, SearchMode::default());
    assert_eq!((info.method, info.deleted), ("hnsw", 1));
    assert!(hits.iter().all(|h| h.0 != n42));
    // the snapshot from before the delete still finds it
    let (old, _) = search(&before_delete, &q, 5, wide);
    assert!(old.iter().any(|h| h.0 == n42));
    update(
        &s,
        &del.replace("DELETE", "INSERT"),
        &QueryOptions::default(),
    )
    .unwrap();
    let (hits, info) = search(&s.snapshot(), &q, 5, wide);
    assert_eq!((info.deleted, info.inserted), (0, 1));
    assert_eq!(hits[0].0, n42);
    // the same through SPARQL, with the per-query options
    let sparql = |opts: &str| {
        let r = query(
            s.snapshot(),
            &format!(
                "SELECT ?s {{ (?s ?score) <urn:x-sparkles:vectorSearch> (<{EMB}> <urn:n42> 3 {opts}) }} ORDER BY DESC(?score)"
            ),
            &QueryOptions::default(),
        )
        .unwrap();
        r.rows()
            .into_iter()
            .map(|r| r[0].as_ref().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(sparql("\"ef:200\""), sparql("\"exact:true\""));
    assert_eq!(sparql("")[0], "<urn:n42>");
    // after a compaction the new generation is indexed again, with the delta folded in
    s.compact().unwrap();
    let st = s.wait_vector_index("emb").unwrap();
    assert_eq!(st.state, "ready");
    assert_eq!(
        (st.rows, st.overlay.inserts, st.overlay.deletes),
        (5_001, 0, 0)
    );
    let (hits, info) = search(&s.snapshot(), &near, 2, SearchMode::default());
    assert_eq!((info.method, info.inserted), ("hnsw", 0));
    assert_eq!(hits[0].0, s.snapshot().lookup_iri("urn:n900001").unwrap().0);
}

#[test]
fn many_random_changes_match_the_oracle() {
    let all = clustered(3_400, 11);
    let (vs, extra) = all.split_at(3_000);
    let s = store_with(Store::in_memory(StoreOptions::default()), vs);
    // A parallel build's graph depends on how its threads interleave, so on the machine's
    // load, and one query's recall@10 then varies between 0.8 and 1 from run to run. The
    // graph is built one node at a time instead, so every run checks the same graph.
    s.sequential_vector_builds(true);
    s.create_vector_index("emb", config(0)).unwrap();
    s.wait_vector_index("emb").unwrap();
    let mut z = 5u64;
    let mut present: Vec<bool> = vec![true; 3_000];
    for round in 0..40 {
        // delete some base rows, re-insert some, add new ones
        let mut ops = String::new();
        for _ in 0..10 {
            let i = (splitmix(&mut z) * 3_000.0) as usize;
            let quad = format!(
                "<urn:n{i}> <{EMB}> \"{}\"^^<urn:x-sparkles:vector> .",
                vector::canonical(&vs[i])
            );
            ops += &if present[i] {
                format!("DELETE DATA {{ {quad} }} ;\n")
            } else {
                format!("INSERT DATA {{ {quad} }} ;\n")
            };
            present[i] = !present[i];
        }
        let e = &extra[round * 10];
        ops += &format!(
            "INSERT DATA {{ <urn:x{round}> <{EMB}> \"{}\"^^<urn:x-sparkles:vector> }}",
            vector::canonical(e)
        );
        update(&s, &ops, &QueryOptions::default()).unwrap();
        let snap = s.snapshot();
        let q = &extra[round * 10 + 1];
        let (exact, _) = search(&snap, q, 10, EXACT);
        let (ann, info) = search(
            &snap,
            q,
            10,
            SearchMode {
                exact: false,
                ef: Some(200),
            },
        );
        assert_eq!(info.method, "hnsw");
        // nothing deleted ever appears
        for (i, p) in present.iter().enumerate() {
            if !p {
                let id = snap.lookup_iri(&format!("urn:n{i}")).unwrap().0;
                assert!(ann.iter().all(|h| h.0 != id));
            }
        }
        // a row's score does not depend on the path
        for a in &ann {
            if let Some(e) = exact.iter().find(|e| e.0 == a.0) {
                assert_eq!(a.1.to_bits(), e.1.to_bits());
            }
        }
        // a wide search finds nearly all of the exact result
        assert!(recall(&ann, &exact) >= 0.9, "round {round}");
        // inserted rows are scored exactly: the new one is found for itself
        let (own, _) = search(&snap, e, 1, SearchMode::default());
        assert_eq!(
            own[0].0,
            snap.lookup_iri(&format!("urn:x{round}")).unwrap().0
        );
    }
}

#[test]
fn past_states_are_searched_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let vs = clustered(2_000, 21);
    let s = store_with(
        Store::open(dir.path(), StoreOptions::default()).unwrap(),
        &vs,
    );
    s.create_vector_index("emb", config(0)).unwrap();
    s.wait_vector_index("emb").unwrap();
    let head = s.snapshot().commit;
    load(&s, nt(&[(777_777, &vs[5])]));
    let (past, _) = s
        .snapshot_at(
            &sparkles_core::history::At::Commit(head),
            &Default::default(),
        )
        .unwrap();
    assert!(past.historical);
    let (hits, info) = search(&past, &vs[5], 3, SearchMode::default());
    assert_eq!(info.method, "exact");
    assert_eq!(info.reason, Some("a past state is searched exactly"));
    let n777 = s.snapshot().lookup_iri("urn:n777777").unwrap().0;
    assert!(hits.iter().all(|h| h.0 != n777));
    let (now, info) = search(&s.snapshot(), &vs[5], 3, SearchMode::default());
    assert_eq!(info.method, "hnsw");
    assert!(now.iter().any(|h| h.0 == n777));
}

#[test]
fn files_are_mapped_on_reopen_and_rebuilt_when_damaged() {
    let dir = tempfile::tempdir().unwrap();
    let vs = clustered(3_000, 31);
    let q = vs[9].clone();
    let first;
    {
        let s = store_with(
            Store::open(dir.path(), StoreOptions::default()).unwrap(),
            &vs,
        );
        s.create_vector_index("emb", config(0)).unwrap();
        let st = s.wait_vector_index("emb").unwrap();
        assert_eq!(
            st.memory.residency,
            "mmap",
            "{st:?} {:?}",
            std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect::<Vec<_>>()
        );
        assert!(!st.files.unwrap().opened);
        first = search(&s.snapshot(), &q, 10, SearchMode::default()).0;
        assert!(dir.path().join("vector.json").exists());
    }
    let file = |d: &std::path::Path| {
        let cur = std::fs::read_to_string(d.join("CURRENT")).unwrap();
        d.join(cur.trim()).join("vectors").join("emb.spkv")
    };
    let path = file(dir.path());
    let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    {
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let st = s.wait_vector_index("emb").unwrap();
        assert_eq!(st.state, "ready");
        assert!(st.files.unwrap().opened, "{st:?}");
        assert_eq!(st.rows, 3_000);
        let (again, info) = search(&s.snapshot(), &q, 10, SearchMode::default());
        assert_eq!(info.method, "hnsw");
        assert_eq!(
            again
                .iter()
                .map(|h| (h.0, h.1.to_bits()))
                .collect::<Vec<_>>(),
            first
                .iter()
                .map(|h| (h.0, h.1.to_bits()))
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    // a damaged id section fails the open checksum: the index is built again
    let mut b = std::fs::read(&path).unwrap();
    let n = b.len();
    b[n / 3] ^= 0xFF;
    b.truncate(n - 1);
    std::fs::write(&path, &b).unwrap();
    {
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let st = s.wait_vector_index("emb").unwrap();
        assert_eq!(st.state, "ready");
        assert!(!st.files.unwrap().opened);
        assert_eq!(st.rows, 3_000);
        // a new efSearch keeps the build; a new M builds again
        let mut c = config(0);
        c.hnsw.as_mut().unwrap().ef_search = 100;
        assert!(!s.create_vector_index("emb", c.clone()).unwrap());
        let st = s.vector_index("emb").unwrap();
        assert_eq!(
            (st.state.as_str(), st.hnsw.unwrap().ef_search),
            ("ready", 100)
        );
        c.hnsw.as_mut().unwrap().m = 6;
        s.create_vector_index("emb", c).unwrap();
        let st = s.wait_vector_index("emb").unwrap();
        assert_eq!((st.state.as_str(), st.hnsw.unwrap().m), ("ready", 6));
        s.drop_vector_index("emb").unwrap();
        assert!(!file(dir.path()).exists());
        assert!(!dir.path().join("vector.json").exists());
        assert!(s.vector_indexes().is_empty());
    }
}

#[test]
fn builds_run_while_writes_go_on() {
    let vs = clustered(2_000, 41);
    let s = store_with(Store::in_memory(StoreOptions::default()), &vs);
    s.pause_vector_builds(true);
    s.create_vector_index("emb", config(0)).unwrap();
    assert_eq!(s.vector_index("emb").unwrap().state, "building");
    // searches answer exactly meanwhile, and writes go on
    let (hits, info) = search(&s.snapshot(), &vs[3], 1, SearchMode::default());
    assert_eq!(info.method, "exact");
    assert_eq!(hits[0].0, s.snapshot().lookup_iri("urn:n3").unwrap().0);
    let near: Vec<f32> = (0..DIM)
        .map(|i| if i % 3 == 0 { 3.0 } else { -3.0 })
        .collect();
    load(&s, nt(&[(123_456, &near)]));
    s.pause_vector_builds(false);
    let st = s.wait_vector_index("emb").unwrap();
    assert_eq!(st.state, "ready");
    assert_eq!((st.rows, st.overlay.inserts), (2_000, 1));
    let (hits, info) = search(&s.snapshot(), &near, 1, SearchMode::default());
    assert_eq!(info.method, "hnsw");
    assert_eq!(hits[0].0, s.snapshot().lookup_iri("urn:n123456").unwrap().0);
}

#[test]
fn configuration_errors() {
    let s = store_with(
        Store::in_memory(StoreOptions::default()),
        &clustered(100, 1),
    );
    s.create_vector_index("emb", config(0)).unwrap();
    s.wait_vector_index("emb").unwrap();
    let e = s.create_vector_index("other", config(0)).unwrap_err();
    assert!(matches!(e, sparkles_core::Error::Conflict(_)), "{e}");
    assert!(matches!(
        s.drop_vector_index("nope").unwrap_err(),
        sparkles_core::Error::NotFound(_)
    ));
    assert!(s.create_vector_index("bad/name", config(0)).is_err());
    let mut c = config(0);
    c.dimension = 0;
    assert!(
        s.create_vector_index("x", c)
            .unwrap_err()
            .to_string()
            .contains("dimension")
    );
    // a query of another dimension than the index's
    let r = query(
        s.snapshot(),
        &format!(
            "SELECT ?s {{ ?s <urn:x-sparkles:vectorSearch> (<{EMB}> \"[1,2]\"^^<urn:x-sparkles:vector>) }}"
        ),
        &QueryOptions::default(),
    );
    let e = r.err().unwrap().to_string();
    assert!(
        e.contains("dimension mismatch") && e.contains("index emb"),
        "{e}"
    );
    for bad in ["\"ef:0\"", "\"ef:x\"", "\"exact:maybe\""] {
        let r = query(
            s.snapshot(),
            &format!("SELECT ?s {{ ?s <urn:x-sparkles:vectorSearch> (<{EMB}> <urn:n1> 3 {bad}) }}"),
            &QueryOptions::default(),
        );
        assert!(r.is_err(), "{bad}");
    }
}
