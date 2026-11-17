//! Candle's kernels stay on the embedding pool's threads: after
//! `confine_candle_threads`, a batch whose activations pass 65,536 elements starts no
//! thread of Candle's own pool (`candle-bp-*`), and the thread count Candle gives its
//! matrix products is the process's available parallelism, read once. One test in its own
//! binary, because Candle reads the environment once per process.

#![cfg(target_os = "linux")]

use sparkles_embed::testing::tiny_bert;
use sparkles_embed::{Embedder, Kind, ModelSpec, Options};

fn candle_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .filter(|c| c.starts_with("candle-bp"))
        .count()
}

#[test]
fn large_batches_start_no_candle_pool() {
    // SAFETY: the only test of this binary, run before it starts any thread of its own
    unsafe { sparkles_embed::confine_candle_threads() };
    let tmp = tempfile::tempdir().unwrap();
    tiny_bert(tmp.path());
    let e = Embedder::new(
        ModelSpec::new(tmp.path()),
        Options {
            threads: 2,
            micro_batch: 256,
            ..Options::default()
        },
    )
    .unwrap();
    // one pass over 256 texts of 16 tokens with 32 dimensions and 64 intermediate units:
    // 131,072 elements in each hidden state and 262,144 in the feed-forward activations
    let texts: Vec<String> = (0..256)
        .map(|i| format!("the cat sat on the mat {i} hello world france the cat sat on"))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let v = e.embed(&refs, Kind::Document).unwrap();
    assert_eq!(v.len(), 256);
    assert_eq!(candle_threads(), 0);
    if std::env::var_os("RAYON_NUM_THREADS").is_none() {
        assert_eq!(
            candle_core::utils::get_num_threads(),
            std::thread::available_parallelism().unwrap().get()
        );
    }
}
