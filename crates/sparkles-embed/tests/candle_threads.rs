//! Candle's kernels stay on the embedding pool's threads: after
//! `confine_candle_threads`, a batch whose activations pass 65,536 elements starts no
//! thread of Candle's own pool (`candle-bp-*`), and the thread count Candle gives its
//! matrix products is the process's available parallelism, read once. A test without
//! libtest's harness, so `main` writes the environment before any thread exists, and in
//! its own binary, because Candle reads the environment once per process. It answers
//! the `--list` of cargo-nextest with its one test, `confine`.

use sparkles_embed::testing::tiny_bert;
use sparkles_embed::{Embedder, Kind, ModelSpec, Options};

#[cfg(target_os = "linux")]
fn candle_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .filter(|c| c.starts_with("candle-bp"))
        .count()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list") {
        if !args.iter().any(|a| a == "--ignored") {
            println!("confine: test");
        }
        return;
    }
    // a filter that names another test selects nothing here
    if args
        .iter()
        .any(|a| !a.starts_with('-') && !"confine".contains(a.as_str()))
    {
        return;
    }
    let preset = |v: &str| std::env::var_os(v).is_some();
    let (threads_set, mask_set, rayon_set) = (
        preset("CANDLE_NUM_THREADS"),
        preset("CANDLE_CPU_MASK"),
        preset("RAYON_NUM_THREADS"),
    );
    // SAFETY: the first statement of main, before this process starts any thread
    unsafe { sparkles_embed::confine_candle_threads() };
    if !threads_set {
        assert_eq!(std::env::var("CANDLE_NUM_THREADS").unwrap(), "1");
    }
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
    #[cfg(target_os = "linux")]
    if !threads_set {
        assert_eq!(candle_threads(), 0);
    }
    #[cfg(target_os = "linux")]
    if !mask_set && !rayon_set {
        assert_eq!(
            candle_core::utils::get_num_threads(),
            std::thread::available_parallelism().unwrap().get()
        );
    }
    println!("test confine ... ok");
}
