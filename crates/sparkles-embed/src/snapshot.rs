//! Reading a sentence-transformers snapshot: the model's `config.json`, `modules.json`
//! with its pooling and normalization modules, `sentence_bert_config.json`,
//! `config_sentence_transformers.json` (prompts), `tokenizer_config.json` and the
//! safetensors weights.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::{Error, Pooling, Result};

/// The architectures this crate runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    Bert,
    XlmRoberta,
    NomicBert,
    Qwen3,
}

impl Arch {
    /// Decoder models read the last token and pad on the left.
    pub fn is_decoder(self) -> bool {
        matches!(self, Arch::Qwen3)
    }

    pub fn name(self) -> &'static str {
        match self {
            Arch::Bert => "bert",
            Arch::XlmRoberta => "xlm-roberta",
            Arch::NomicBert => "nomic_bert",
            Arch::Qwen3 => "qwen3",
        }
    }
}

/// What a snapshot says about how to embed.
#[derive(Clone, Debug)]
pub struct SnapshotConfig {
    pub dir: PathBuf,
    pub arch: Arch,
    /// `config.json` as read.
    pub config: Value,
    pub hidden_size: usize,
    /// From the Pooling module, or `None` when the snapshot has no `modules.json`.
    pub pooling: Option<Pooling>,
    /// Whether the Pooling module counts prompt tokens (sentence-transformers'
    /// `include_prompt`).
    pub include_prompt: bool,
    /// Whether `modules.json` lists a Normalize module (`None` without `modules.json`).
    pub normalize: Option<bool>,
    /// `max_seq_length` of `sentence_bert_config.json`.
    pub max_seq_length: Option<usize>,
    pub max_position_embeddings: Option<usize>,
    pub do_lower_case: bool,
    pub query_prompt: Option<String>,
    pub document_prompt: Option<String>,
    /// `padding_side` of `tokenizer_config.json`.
    pub left_padding: Option<bool>,
    pub eos_token_id: Option<u32>,
    pub pad_token_id: Option<u32>,
    pub weights: Vec<PathBuf>,
    pub tokenizer: PathBuf,
}

#[derive(Deserialize)]
struct Module {
    #[serde(default)]
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

/// The Pooling module's `config.json` (sentence-transformers `models.Pooling`).
#[derive(Deserialize, Default)]
#[serde(default)]
struct PoolingConfig {
    word_embedding_dimension: Option<usize>,
    pooling_mode_cls_token: bool,
    pooling_mode_mean_tokens: bool,
    pooling_mode_max_tokens: bool,
    pooling_mode_mean_sqrt_len_tokens: bool,
    pooling_mode_weightedmean_tokens: bool,
    pooling_mode_lasttoken: bool,
    include_prompt: Option<bool>,
}

fn read_json(path: &Path) -> Result<Option<Value>> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| Error::Snapshot(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Snapshot(format!("{}: {e}", path.display()))),
    }
}

fn as_u32(v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::Number(n) => n.as_u64().map(|n| n as u32),
        // some configs list several EOS ids; the first is the one tokenizers append
        Value::Array(a) => a.first().and_then(|n| n.as_u64()).map(|n| n as u32),
        _ => None,
    }
}

/// The files of a Hub repository an embedding model needs: the root JSON files, the
/// tokenizer, the safetensors weights (one file or shards) and the configuration of each
/// sentence-transformers module directory. ONNX, OpenVINO, PyTorch pickles and other
/// formats are skipped.
pub fn hub_select(path: &str) -> bool {
    let (dir, file) = match path.rsplit_once('/') {
        Some((d, f)) => (Some(d), f),
        None => (None, path),
    };
    match dir {
        None => {
            file.ends_with(".json") && !file.starts_with("onnx")
                || file == "model.safetensors"
                || (file.starts_with("model-") && file.ends_with(".safetensors"))
                || file == "tokenizer.model"
        }
        // `1_Pooling/config.json`, `2_Normalize` holds no file
        Some(d) => {
            !d.contains('/')
                && d.split_once('_')
                    .is_some_and(|(n, _)| n.parse::<u32>().is_ok())
                && file == "config.json"
        }
    }
}

impl SnapshotConfig {
    /// Read the snapshot in `dir`. Refuses an architecture this crate does not run, a
    /// module it does not implement, and weights in a format other than safetensors.
    pub fn read(dir: &Path) -> Result<SnapshotConfig> {
        let config = read_json(&dir.join("config.json"))?
            .ok_or_else(|| Error::Snapshot(format!("{}: no config.json", dir.display())))?;
        let model_type = config
            .get("model_type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let arch = match model_type.as_str() {
            "bert" => Arch::Bert,
            "xlm-roberta" => Arch::XlmRoberta,
            "nomic_bert" => Arch::NomicBert,
            "qwen3" => Arch::Qwen3,
            other => {
                return Err(Error::Unsupported(format!(
                    "model_type {other:?}; supported: bert, xlm-roberta, nomic_bert, qwen3"
                )));
            }
        };
        let hidden_size = config
            .get("hidden_size")
            .or_else(|| config.get("n_embd"))
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::Snapshot("config.json has no hidden_size".into()))?
            as usize;

        let mut pooling = None;
        let mut include_prompt = true;
        let mut normalize = None;
        if let Some(m) = read_json(&dir.join("modules.json"))? {
            let modules: Vec<Module> = serde_json::from_value(m)
                .map_err(|e| Error::Snapshot(format!("modules.json: {e}")))?;
            let mut norm = false;
            for m in &modules {
                let kind = m.kind.rsplit('.').next().unwrap_or("");
                match kind {
                    "Transformer" => {}
                    "Normalize" => norm = true,
                    "Pooling" => {
                        let p = dir.join(&m.path).join("config.json");
                        let v = read_json(&p)?
                            .ok_or_else(|| Error::Snapshot(format!("{}: missing", p.display())))?;
                        let pc: PoolingConfig = serde_json::from_value(v)
                            .map_err(|e| Error::Snapshot(format!("{}: {e}", p.display())))?;
                        if let Some(d) = pc.word_embedding_dimension
                            && d != hidden_size
                        {
                            return Err(Error::Snapshot(format!(
                                "the Pooling module expects dimension {d}, the model has {hidden_size}"
                            )));
                        }
                        include_prompt = pc.include_prompt.unwrap_or(true);
                        pooling = Some(pooling_of(&pc)?);
                    }
                    other => {
                        return Err(Error::Unsupported(format!(
                            "sentence-transformers module {other:?} ({})",
                            m.kind
                        )));
                    }
                }
            }
            normalize = Some(norm);
        }

        let sbert = read_json(&dir.join("sentence_bert_config.json"))?;
        let max_seq_length = sbert
            .as_ref()
            .and_then(|v| v.get("max_seq_length"))
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        let do_lower_case = sbert
            .as_ref()
            .and_then(|v| v.get("do_lower_case"))
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let (mut query_prompt, mut document_prompt) = (None, None);
        if let Some(st) = read_json(&dir.join("config_sentence_transformers.json"))?
            && let Some(Value::Object(p)) = st.get("prompts")
        {
            let get = |keys: &[&str]| {
                keys.iter()
                    .find_map(|k| p.get(*k).and_then(Value::as_str))
                    .map(str::to_string)
            };
            query_prompt = get(&["query", "search_query"]);
            document_prompt = get(&["document", "passage", "search_document"]);
        }

        let tc = read_json(&dir.join("tokenizer_config.json"))?;
        let left_padding = tc
            .as_ref()
            .and_then(|v| v.get("padding_side"))
            .and_then(Value::as_str)
            .map(|s| s == "left");

        let tokenizer = dir.join("tokenizer.json");
        if !tokenizer.is_file() {
            return Err(Error::Snapshot(format!(
                "{}: no tokenizer.json (only the fast tokenizer format is read)",
                dir.display()
            )));
        }
        let weights = weight_files(dir)?;
        Ok(SnapshotConfig {
            dir: dir.to_path_buf(),
            arch,
            hidden_size,
            pooling,
            include_prompt,
            normalize,
            max_seq_length,
            max_position_embeddings: config
                .get("max_position_embeddings")
                .or_else(|| config.get("n_positions"))
                .and_then(Value::as_u64)
                .map(|n| n as usize),
            do_lower_case,
            query_prompt,
            document_prompt,
            left_padding,
            eos_token_id: as_u32(config.get("eos_token_id")),
            pad_token_id: as_u32(config.get("pad_token_id")),
            weights,
            tokenizer,
            config,
        })
    }
}

fn pooling_of(pc: &PoolingConfig) -> Result<Pooling> {
    let modes = [
        (pc.pooling_mode_cls_token, Pooling::Cls),
        (pc.pooling_mode_mean_tokens, Pooling::Mean),
        (pc.pooling_mode_max_tokens, Pooling::Max),
        (pc.pooling_mode_mean_sqrt_len_tokens, Pooling::MeanSqrtLen),
        (pc.pooling_mode_lasttoken, Pooling::LastToken),
    ];
    if pc.pooling_mode_weightedmean_tokens {
        return Err(Error::Unsupported("weighted-mean pooling".into()));
    }
    let on: Vec<Pooling> = modes.iter().filter(|(b, _)| *b).map(|(_, p)| *p).collect();
    match on.as_slice() {
        [p] => Ok(*p),
        [] => Err(Error::Snapshot("the Pooling module enables no mode".into())),
        _ => Err(Error::Unsupported(
            "several pooling modes at once (concatenated outputs)".into(),
        )),
    }
}

fn weight_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let single = dir.join("model.safetensors");
    if single.is_file() {
        return Ok(vec![single]);
    }
    let index = dir.join("model.safetensors.index.json");
    if let Some(v) = read_json(&index)? {
        let mut files: Vec<String> = v
            .get("weight_map")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                Error::Snapshot("model.safetensors.index.json has no weight_map".into())
            })?
            .values()
            .filter_map(|f| f.as_str().map(str::to_string))
            .collect();
        files.sort();
        files.dedup();
        let mut out = Vec::new();
        for f in files {
            sparkles_modelstore::check_rel_path(&f).map_err(|e| Error::Snapshot(e.to_string()))?;
            out.push(dir.join(f));
        }
        return Ok(out);
    }
    Err(Error::Unsupported(format!(
        "{}: no model.safetensors; only safetensors weights are read",
        dir.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection() {
        for p in [
            "config.json",
            "tokenizer.json",
            "modules.json",
            "model.safetensors",
            "model-00001-of-00002.safetensors",
            "1_Pooling/config.json",
        ] {
            assert!(hub_select(p), "{p}");
        }
        for p in [
            "pytorch_model.bin",
            "onnx/model.onnx",
            "openvino/openvino_model.xml",
            "README.md",
            "2_Dense/model.safetensors",
            "a/b/config.json",
        ] {
            assert!(!hub_select(p), "{p}");
        }
    }
}
