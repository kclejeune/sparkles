//! Real models from local snapshots, ignored by default. Set the directories and run
//! with `--ignored`:
//!
//! ```sh
//! SPARKLES_EMBED_MINILM=…/sentence-transformers/all-MiniLM-L6-v2/<rev> \
//! SPARKLES_EMBED_QWEN3=…/Qwen/Qwen3-Embedding-0.6B/<rev> \
//! SPARKLES_EMBED_NOMIC=…/nomic-ai/nomic-embed-text-v1.5/<rev> \
//!   cargo test --release -p sparkles-embed --test real -- --ignored
//! ```

use sparkles_embed::pooling::cosine;
use sparkles_embed::{Dtype, Embedder, Kind, ModelSpec, Options};

fn dir(var: &str) -> Option<String> {
    let d = std::env::var(var).ok();
    if d.is_none() {
        eprintln!("{var} is not set; skipped");
    }
    d
}

fn paraphrases(e: &Embedder) {
    let v = e
        .embed(
            &[
                "A man is playing a guitar.",
                "Someone is strumming a guitar.",
                "The stock market fell sharply today.",
            ],
            Kind::Document,
        )
        .unwrap();
    let (para, other) = (cosine(&v[0], &v[1]), cosine(&v[0], &v[2]));
    eprintln!("paraphrase {para:.3}, unrelated {other:.3}");
    assert!(para > other + 0.3, "paraphrase {para}, unrelated {other}");
}

#[test]
#[ignore]
fn minilm() {
    let Some(d) = dir("SPARKLES_EMBED_MINILM") else {
        return;
    };
    let e = Embedder::new(ModelSpec::new(d), Options::default()).unwrap();
    assert_eq!(e.info().dimension, 384);
    paraphrases(&e);
}

/// The model card's example: two queries with the web-search instruction against two
/// documents, whose similarities the card prints.
fn qwen3_card(dtype: Dtype) {
    let Some(d) = dir("SPARKLES_EMBED_QWEN3") else {
        return;
    };
    let mut spec = ModelSpec::new(d);
    spec.dtype = dtype;
    let e = Embedder::new(spec, Options::default()).unwrap();
    assert_eq!(e.info().dimension, 1024);
    let q = e
        .embed(
            &["What is the capital of China?", "Explain gravity"],
            Kind::Query,
        )
        .unwrap();
    let d = e
        .embed(
            &[
                "The capital of China is Beijing.",
                "Gravity is a force that attracts two bodies towards each other. It gives weight to physical objects and is responsible for the movement of planets around the sun.",
            ],
            Kind::Document,
        )
        .unwrap();
    let want = [[0.7646, 0.1414], [0.1355, 0.6000]];
    for i in 0..2 {
        for j in 0..2 {
            let s = cosine(&q[i], &d[j]);
            eprintln!("{dtype:?} q{i} d{j}: {s:.4} (card {:.4})", want[i][j]);
            assert!((s - want[i][j]).abs() < 0.02, "q{i} d{j}: {s}");
        }
    }
    paraphrases(&e);
}

#[test]
#[ignore]
fn qwen3_f32() {
    qwen3_card(Dtype::F32);
}

#[test]
#[ignore]
fn qwen3_bf16() {
    qwen3_card(Dtype::Bf16);
}

/// nomic-embed-text-v1.5 with the model card's task prefixes, which the snapshot does not
/// declare. The first values of a query and a document vector were recorded from
/// candle-transformers 0.11's `nomic_bert`, the reference this crate's encoder is checked
/// against. bf16 weights must give the same cosine within 5e-3.
fn nomic(dtype: Dtype) {
    let Some(d) = dir("SPARKLES_EMBED_NOMIC") else {
        return;
    };
    let mut spec = ModelSpec::new(d);
    spec.dtype = dtype;
    spec.query_prompt = Some("search_query: ".into());
    spec.document_prompt = Some("search_document: ".into());
    let e = Embedder::new(spec, Options::default()).unwrap();
    assert_eq!(e.info().dimension, 768);
    let q = e.embed(&["What is TSNE?"], Kind::Query).unwrap();
    let d = e
        .embed(
            &["t-SNE is a statistical method for visualizing high-dimensional data."],
            Kind::Document,
        )
        .unwrap();
    let c = cosine(&q[0], &d[0]);
    eprintln!("{dtype:?} query {:?}, cosine {c:.4}", &q[0][..6]);
    let want_q = [
        -0.08891342,
        1.2341346,
        -4.0468693,
        -1.0364419,
        0.85301375,
        0.14920467,
    ];
    let want_d = [
        0.30549204,
        1.2516627,
        -4.1372876,
        -1.6681005,
        0.61639684,
        -0.05138422,
    ];
    if dtype == Dtype::F32 {
        for (got, want) in [(&q[0], want_q), (&d[0], want_d)] {
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 2e-3, "{g} against {w}");
            }
        }
        assert!((c - 0.7725).abs() < 1e-3, "cosine {c}");
    } else {
        assert!((c - 0.7725).abs() < 5e-3, "cosine {c}");
    }
    paraphrases(&e);
}

#[test]
#[ignore]
fn nomic_f32() {
    nomic(Dtype::F32);
}

#[test]
#[ignore]
fn nomic_bf16() {
    nomic(Dtype::Bf16);
}
