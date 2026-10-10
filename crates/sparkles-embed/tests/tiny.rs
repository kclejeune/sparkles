//! Tiny models with random weights, written by the tests: loading, pooling, padding,
//! prompts, truncation, idle unloading, the queue and the refusals. No network.

use std::path::Path;
use std::time::Duration;

use candle_core::{DType, Device, Tensor};
use candle_nn::{VarBuilder, VarMap};
use serde_json::json;
use sparkles_embed::pooling::{cosine, normalize};
use sparkles_embed::testing::{
    WORDS, bert_config, modules, tiny_bert, write_json, write_tokenizer,
};
use sparkles_embed::{Dtype, Embedder, Error, Kind, ModelSpec, Options, State};

fn qwen3_config() -> serde_json::Value {
    json!({
        "model_type": "qwen3", "architectures": ["Qwen3ForCausalLM"],
        "vocab_size": WORDS.len(), "hidden_size": 32, "intermediate_size": 64,
        "num_hidden_layers": 2, "num_attention_heads": 4, "num_key_value_heads": 2,
        "head_dim": 8, "attention_bias": false, "max_position_embeddings": 128,
        "sliding_window": null, "max_window_layers": 2, "tie_word_embeddings": true,
        "rope_theta": 10000.0, "rms_norm_eps": 1e-6, "use_sliding_window": false,
        "hidden_act": "silu", "eos_token_id": 4, "pad_token_id": 0
    })
}

/// A two-layer Qwen3 with random weights written through Candle's own Qwen3 model, so
/// the tensor names are the checkpoints' (`model.` prefix), last-token pooling, prompts.
fn tiny_qwen3(dir: &Path) -> VarMap {
    let cfg = qwen3_config();
    write_json(&dir.join("config.json"), cfg.clone());
    let vm = VarMap::new();
    let vb = VarBuilder::from_varmap(&vm, DType::F32, &Device::Cpu);
    let c: candle_transformers::models::qwen3::Config = serde_json::from_value(cfg).unwrap();
    candle_transformers::models::qwen3::Model::new(&c, vb).unwrap();
    // RMSNorm weights start at one; perturb them so a wrong norm shows
    for (name, var) in vm.data().lock().unwrap().iter() {
        if name.ends_with("norm.weight") {
            let t = (Tensor::randn(0f32, 0.1, var.shape(), &Device::Cpu).unwrap() + 1.0).unwrap();
            var.set(&t).unwrap();
        }
    }
    vm.save(dir.join("model.safetensors")).unwrap();
    write_tokenizer(dir, false);
    modules(dir, "lasttoken", true);
    write_json(
        &dir.join("tokenizer_config.json"),
        json!({"padding_side": "left"}),
    );
    write_json(
        &dir.join("config_sentence_transformers.json"),
        json!({"prompts": {"query": "instruct query: ", "document": ""}, "default_prompt_name": null}),
    );
    vm
}

fn opts() -> Options {
    Options {
        threads: 2,
        ..Options::default()
    }
}

#[test]
fn bert_mean_pooling_matches_a_direct_forward() {
    let tmp = tempfile::tempdir().unwrap();
    tiny_bert(tmp.path());
    let e = Embedder::new(ModelSpec::new(tmp.path()), opts()).unwrap();
    assert_eq!(e.info().arch, "bert");
    assert_eq!(e.info().dimension, 32);
    assert_eq!(e.info().max_tokens, 16);
    assert_eq!(e.status().state, State::Unloaded);
    let texts = ["hello world", "the cat sat on the mat", "france"];
    let batch = e.embed(&texts, Kind::Document).unwrap();
    assert_eq!(e.status().state, State::Loaded);
    // padding does not change a text's vector
    for (t, v) in texts.iter().zip(&batch) {
        let one = e.embed(&[t], Kind::Document).unwrap();
        assert!(cosine(&one[0], v) > 0.99999, "{t}");
        let n: f32 = v.iter().map(|x| x * x).sum();
        assert!((n - 1.0).abs() < 1e-5);
    }
    // the same as Candle's BERT followed by a mean and a normalization by hand
    let cfg: candle_transformers::models::bert::Config =
        serde_json::from_value(bert_config()).unwrap();
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(
            &[tmp.path().join("model.safetensors")],
            DType::F32,
            &Device::Cpu,
        )
    }
    .unwrap();
    let m = candle_transformers::models::bert::BertModel::load(vb, &cfg).unwrap();
    let ids = Tensor::new(&[[2u32, 5, 6, 3]], &Device::Cpu).unwrap();
    let out = m.forward(&ids, &ids.zeros_like().unwrap(), None).unwrap();
    let mut mean: Vec<f32> = out
        .mean(1)
        .unwrap()
        .flatten_all()
        .unwrap()
        .to_vec1()
        .unwrap();
    normalize(&mut mean);
    assert!(cosine(&mean, &batch[0]) > 0.99999);
    for (a, b) in mean.iter().zip(&batch[0]) {
        assert!((a - b).abs() < 1e-5);
    }
}

#[test]
fn qwen3_last_token_matches_candle_and_ignores_padding() {
    let tmp = tempfile::tempdir().unwrap();
    tiny_qwen3(tmp.path());
    let e = Embedder::new(ModelSpec::new(tmp.path()), opts()).unwrap();
    assert_eq!(e.info().arch, "qwen3");
    assert_eq!(e.info().query_prompt, "instruct query: ");
    let texts = ["hello world", "the cat sat on the mat of france", "a"];
    let batch = e.embed(&texts, Kind::Document).unwrap();
    for (t, v) in texts.iter().zip(&batch) {
        let one = e.embed(&[t], Kind::Document).unwrap();
        assert!(cosine(&one[0], v) > 0.9999, "{t}: {}", cosine(&one[0], v));
    }
    // Candle's causal Qwen3 on the unpadded ids with the end-of-text token appended
    let cfg: candle_transformers::models::qwen3::Config =
        serde_json::from_value(qwen3_config()).unwrap();
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(
            &[tmp.path().join("model.safetensors")],
            DType::F32,
            &Device::Cpu,
        )
    }
    .unwrap();
    let mut m = candle_transformers::models::qwen3::Model::new(&cfg, vb).unwrap();
    let ids = Tensor::new(&[[5u32, 6, 4]], &Device::Cpu).unwrap();
    let out = m.forward(&ids, 0).unwrap();
    let mut last: Vec<f32> = out
        .narrow(1, 2, 1)
        .unwrap()
        .flatten_all()
        .unwrap()
        .to_vec1()
        .unwrap();
    normalize(&mut last);
    for (a, b) in last.iter().zip(&batch[0]) {
        assert!((a - b).abs() < 1e-4, "{a} {b}");
    }
    // queries carry the instruction
    let q = e.embed(&["hello world"], Kind::Query).unwrap();
    assert!(cosine(&q[0], &batch[0]) < 0.9999);
}

#[test]
fn qwen3_bf16_is_close_to_f32() {
    let tmp = tempfile::tempdir().unwrap();
    tiny_qwen3(tmp.path());
    let f = Embedder::new(ModelSpec::new(tmp.path()), opts()).unwrap();
    let mut spec = ModelSpec::new(tmp.path());
    spec.dtype = Dtype::Bf16;
    let b = Embedder::new(spec, opts()).unwrap();
    let t = ["the cat sat on the mat", "rivers of france"];
    let (vf, vb) = (
        f.embed(&t, Kind::Document).unwrap(),
        b.embed(&t, Kind::Document).unwrap(),
    );
    for (x, y) in vf.iter().zip(&vb) {
        assert!(cosine(x, y) > 0.99, "{}", cosine(x, y));
    }
    assert_eq!(
        b.status().weight_bytes.unwrap() * 2,
        f.status().weight_bytes.unwrap()
    );
}

#[test]
fn matryoshka_truncation() {
    let tmp = tempfile::tempdir().unwrap();
    tiny_bert(tmp.path());
    let full = Embedder::new(ModelSpec::new(tmp.path()), opts()).unwrap();
    let mut spec = ModelSpec::new(tmp.path());
    spec.dimension = Some(8);
    let short = Embedder::new(spec, opts()).unwrap();
    let a = full.embed(&["hello world"], Kind::Query).unwrap().remove(0);
    let b = short
        .embed(&["hello world"], Kind::Query)
        .unwrap()
        .remove(0);
    assert_eq!(b.len(), 8);
    let mut want = a[..8].to_vec();
    normalize(&mut want);
    for (x, y) in want.iter().zip(&b) {
        assert!((x - y).abs() < 1e-6);
    }
    let mut spec = ModelSpec::new(tmp.path());
    spec.dimension = Some(33);
    assert!(matches!(
        Embedder::new(spec, opts()),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn unloads_after_idle_and_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    tiny_bert(tmp.path());
    let o = Options {
        idle_unload: Some(Duration::from_millis(200)),
        ..opts()
    };
    let e = Embedder::new(ModelSpec::new(tmp.path()), o).unwrap();
    e.embed(&["hello"], Kind::Query).unwrap();
    let s = e.status();
    assert_eq!((s.state, s.loads), (State::Loaded, 1));
    assert!(s.weight_bytes.unwrap() > 0);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while e.status().state != State::Unloaded {
        assert!(std::time::Instant::now() < deadline, "not unloaded");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(e.status().unloads, 1);
    assert!(e.status().weight_bytes.is_none());
    e.embed(&["hello"], Kind::Query).unwrap();
    assert_eq!(e.status().loads, 2);
    e.unload();
    while e.status().state != State::Unloaded {
        assert!(
            std::time::Instant::now() < deadline,
            "not unloaded on request"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn queue_limit_and_query_priority() {
    let tmp = tempfile::tempdir().unwrap();
    tiny_bert(tmp.path());
    let o = Options {
        max_queued: 2,
        ..opts()
    };
    let e = Embedder::new(ModelSpec::new(tmp.path()), o).unwrap();
    assert!(matches!(
        e.embed(&["a", "b", "c"], Kind::Document),
        Err(Error::Busy)
    ));
    e.embed(&["a", "b"], Kind::Document).unwrap();

    let o = Options {
        micro_batch: 1,
        ..opts()
    };
    let e = std::sync::Arc::new(Embedder::new(ModelSpec::new(tmp.path()), o).unwrap());
    e.embed(&["warm"], Kind::Query).unwrap();
    let docs: Vec<String> = (0..400)
        .map(|i| format!("the cat sat on the mat {i}"))
        .collect();
    let e2 = e.clone();
    let worker = std::thread::spawn(move || {
        let refs: Vec<&str> = docs.iter().map(String::as_str).collect();
        e2.embed(&refs, Kind::Document).unwrap().len()
    });
    while e.status().queued_documents == 0 {
        std::thread::yield_now();
    }
    e.embed(&["rivers of france"], Kind::Query).unwrap();
    // the query came back while the documents were still in progress
    assert!(
        e.status().queued_documents > 0,
        "the document job finished first"
    );
    assert_eq!(worker.join().unwrap(), 400);
}

#[test]
fn refusals() {
    let tmp = tempfile::tempdir().unwrap();
    tiny_bert(tmp.path());
    let mut spec = ModelSpec::new(tmp.path());
    spec.dtype = Dtype::Bf16;
    assert!(matches!(
        Embedder::new(spec, opts()),
        Err(Error::Unsupported(_))
    ));

    let mut cfg = bert_config();
    cfg["model_type"] = json!("gpt2");
    write_json(&tmp.path().join("config.json"), cfg);
    assert!(matches!(
        Embedder::new(ModelSpec::new(tmp.path()), opts()),
        Err(Error::Unsupported(_))
    ));
    write_json(&tmp.path().join("config.json"), bert_config());

    // a Dense module is not implemented
    let mut m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(tmp.path().join("modules.json")).unwrap()).unwrap();
    m.as_array_mut()
        .unwrap()
        .push(json!({"idx": 3, "name": "3", "path": "3_Dense", "type": "sentence_transformers.models.Dense"}));
    write_json(&tmp.path().join("modules.json"), m);
    assert!(matches!(
        Embedder::new(ModelSpec::new(tmp.path()), opts()),
        Err(Error::Unsupported(_))
    ));

    // without modules.json the pooling must be given
    std::fs::remove_file(tmp.path().join("modules.json")).unwrap();
    assert!(matches!(
        Embedder::new(ModelSpec::new(tmp.path()), opts()),
        Err(Error::Snapshot(_))
    ));
    let mut spec = ModelSpec::new(tmp.path());
    spec.pooling = Some(sparkles_embed::Pooling::Cls);
    let e = Embedder::new(spec, opts()).unwrap();
    assert!(e.info().normalize);

    // pickled PyTorch weights are not read
    std::fs::remove_file(tmp.path().join("model.safetensors")).unwrap();
    assert!(matches!(
        Embedder::new(ModelSpec::new(tmp.path()), opts()),
        Err(Error::Unsupported(_))
    ));
}
