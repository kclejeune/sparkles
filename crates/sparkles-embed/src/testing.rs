//! Tiny models with random weights for tests, here and in the server (not a stable API).

use std::collections::HashMap;
use std::path::Path;

use candle_core::{DType, Device};
use candle_nn::{VarBuilder, VarMap};
use serde_json::json;
use tokenizers::Tokenizer;
use tokenizers::models::wordlevel::WordLevel;
use tokenizers::pre_tokenizers::whitespace::Whitespace;
use tokenizers::processors::template::TemplateProcessing;

pub const WORDS: &[&str] = &[
    "[PAD]", "[UNK]", "[CLS]", "[SEP]", "<eos>", "hello", "world", "rivers", "of", "france", "a",
    "cat", "sat", "on", "the", "mat", "query:", "passage:", "instruct",
];

pub fn write_tokenizer(dir: &Path, bert: bool) {
    let vocab: HashMap<String, u32> = WORDS
        .iter()
        .enumerate()
        .map(|(i, w)| (w.to_string(), i as u32))
        .collect();
    let wl = WordLevel::builder()
        .vocab(vocab.into_iter().collect())
        .unk_token("[UNK]".into())
        .build()
        .unwrap();
    let mut tok = Tokenizer::new(wl);
    tok.with_pre_tokenizer(Some(Whitespace {}));
    if bert {
        let tp = TemplateProcessing::builder()
            .try_single("[CLS] $A [SEP]")
            .unwrap()
            .special_tokens(vec![("[CLS]", 2), ("[SEP]", 3)])
            .build()
            .unwrap();
        tok.with_post_processor(Some(tp));
    }
    tok.save(dir.join("tokenizer.json"), false).unwrap();
}

pub fn write_json(path: &Path, v: serde_json::Value) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
}

pub fn modules(dir: &Path, pooling: &str, normalize: bool) {
    let mut m = vec![
        json!({"idx": 0, "name": "0", "path": "", "type": "sentence_transformers.models.Transformer"}),
        json!({"idx": 1, "name": "1", "path": "1_Pooling", "type": "sentence_transformers.models.Pooling"}),
    ];
    if normalize {
        m.push(json!({"idx": 2, "name": "2", "path": "2_Normalize", "type": "sentence_transformers.models.Normalize"}));
    }
    write_json(&dir.join("modules.json"), json!(m));
    let mut p = json!({
        "word_embedding_dimension": 32,
        "pooling_mode_cls_token": false,
        "pooling_mode_mean_tokens": false,
        "pooling_mode_max_tokens": false,
        "pooling_mode_mean_sqrt_len_tokens": false,
        "pooling_mode_weightedmean_tokens": false,
        "pooling_mode_lasttoken": false,
        "include_prompt": true
    });
    p[format!("pooling_mode_{pooling}")] = json!(true);
    write_json(&dir.join("1_Pooling/config.json"), p);
}

pub fn bert_config() -> serde_json::Value {
    json!({
        "model_type": "bert", "architectures": ["BertModel"],
        "vocab_size": WORDS.len(), "hidden_size": 32, "num_hidden_layers": 2,
        "num_attention_heads": 4, "intermediate_size": 64, "hidden_act": "gelu",
        "hidden_dropout_prob": 0.0, "max_position_embeddings": 64, "type_vocab_size": 2,
        "initializer_range": 0.02, "layer_norm_eps": 1e-12, "pad_token_id": 0
    })
}

/// A two-layer BERT with random weights, mean pooling and normalization.
pub fn tiny_bert(dir: &Path) {
    let cfg = bert_config();
    write_json(&dir.join("config.json"), cfg.clone());
    let vm = VarMap::new();
    let vb = VarBuilder::from_varmap(&vm, DType::F32, &Device::Cpu);
    let c: candle_transformers::models::bert::Config = serde_json::from_value(cfg).unwrap();
    candle_transformers::models::bert::BertModel::load(vb, &c).unwrap();
    vm.save(dir.join("model.safetensors")).unwrap();
    write_tokenizer(dir, true);
    modules(dir, "mean_tokens", true);
    write_json(
        &dir.join("sentence_bert_config.json"),
        json!({"max_seq_length": 16, "do_lower_case": false}),
    );
}
