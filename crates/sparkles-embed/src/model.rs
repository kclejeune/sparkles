//! A loaded model: tokenizer, encoder weights and the settings that turn token states into
//! vectors.

use std::time::Instant;

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::{bert, xlm_roberta};
use tokenizers::{Tokenizer, TruncationDirection, TruncationParams, TruncationStrategy};

use crate::nomic::{self, NomicEncoder};
use crate::pooling::{normalize, pool, truncate};
use crate::qwen3::{self, Qwen3Encoder};
use crate::snapshot::{Arch, SnapshotConfig};
use crate::{Dtype, Error, Kind, Pooling, Result};

enum Encoder {
    Bert(bert::BertModel),
    XlmRoberta(xlm_roberta::XLMRobertaModel),
    Nomic(NomicEncoder),
    Qwen3(Qwen3Encoder),
}

/// Everything resolved from the snapshot and the caller's overrides.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub pooling: Pooling,
    pub normalize: bool,
    pub max_tokens: usize,
    pub dimension: usize,
    pub query_prompt: String,
    pub document_prompt: String,
    pub left_padding: bool,
    pub append_eos: Option<u32>,
    pub pad_id: u32,
    pub lowercase: bool,
}

pub(crate) struct Loaded {
    encoder: Encoder,
    tokenizer: Tokenizer,
    pub settings: Settings,
    /// The bytes the weights take in memory at the chosen type.
    pub weight_bytes: u64,
    pub load_ms: u64,
}

fn cerr(e: candle_core::Error) -> Error {
    Error::Model(e.to_string())
}

impl Loaded {
    pub fn load(cfg: &SnapshotConfig, settings: Settings, dtype: Dtype) -> Result<Loaded> {
        let start = Instant::now();
        let dtype = match dtype {
            Dtype::F32 => DType::F32,
            Dtype::Bf16 => DType::BF16,
        };
        let device = Device::Cpu;
        let mut tokenizer = Tokenizer::from_file(&cfg.tokenizer)
            .map_err(|e| Error::Snapshot(format!("tokenizer.json: {e}")))?;
        // the reserved place for an appended end-of-sequence token
        let max = settings.max_tokens - usize::from(settings.append_eos.is_some());
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: max,
                strategy: TruncationStrategy::LongestFirst,
                stride: 0,
                direction: TruncationDirection::Right,
            }))
            .map_err(|e| Error::Snapshot(format!("tokenizer.json: {e}")))?;
        tokenizer.with_padding(None);

        // SAFETY: the files are memory-mapped while the tensors are copied out of them;
        // a snapshot is not modified while it is in use (the store writes new snapshots
        // to a staging directory and never rewrites a completed one).
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&cfg.weights, dtype, &device) }
            .map_err(cerr)?;
        let weight_bytes = {
            let st = unsafe { candle_core::safetensors::MmapedSafetensors::multi(&cfg.weights) }
                .map_err(cerr)?;
            st.tensors()
                .iter()
                .map(|(_, v)| {
                    v.shape().iter().product::<usize>() as u64 * dtype.size_in_bytes() as u64
                })
                .sum()
        };
        let parse = |what: &str, e: serde_json::Error| {
            Error::Snapshot(format!("config.json for {what}: {e}"))
        };
        let encoder = match cfg.arch {
            Arch::Bert => {
                if dtype != DType::F32 {
                    // BERT's extended mask multiplies f32::MIN, -inf in bf16, by zero
                    return Err(Error::Unsupported(
                        "bert runs in f32 only (Candle's padding mask overflows in bf16)".into(),
                    ));
                }
                let c: bert::Config =
                    serde_json::from_value(cfg.config.clone()).map_err(|e| parse("bert", e))?;
                Encoder::Bert(bert::BertModel::load(vb, &c).map_err(cerr)?)
            }
            Arch::XlmRoberta => {
                if dtype != DType::F32 {
                    return Err(Error::Unsupported(
                        "xlm-roberta runs in f32 only (Candle builds its attention mask in f32)"
                            .into(),
                    ));
                }
                let c: xlm_roberta::Config = serde_json::from_value(cfg.config.clone())
                    .map_err(|e| parse("xlm-roberta", e))?;
                let m = xlm_roberta::XLMRobertaModel::new(&c, vb.clone())
                    .or_else(|e| {
                        xlm_roberta::XLMRobertaModel::new(&c, vb.pp("roberta")).map_err(|_| e)
                    })
                    .map_err(cerr)?;
                Encoder::XlmRoberta(m)
            }
            Arch::NomicBert => {
                let c: nomic::Config = serde_json::from_value(cfg.config.clone())
                    .map_err(|e| parse("nomic_bert", e))?;
                Encoder::Nomic(NomicEncoder::load(&c, vb).map_err(cerr)?)
            }
            Arch::Qwen3 => {
                let c: qwen3::Config =
                    serde_json::from_value(cfg.config.clone()).map_err(|e| parse("qwen3", e))?;
                Encoder::Qwen3(Qwen3Encoder::load(&c, vb).map_err(cerr)?)
            }
        };
        Ok(Loaded {
            encoder,
            tokenizer,
            settings,
            weight_bytes,
            load_ms: start.elapsed().as_millis() as u64,
        })
    }

    /// Embed one batch. The caller keeps batches small; the batch is padded to its
    /// longest text.
    pub fn embed(&self, texts: &[&str], kind: Kind) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let s = &self.settings;
        let prompt = match kind {
            Kind::Query => s.query_prompt.as_str(),
            Kind::Document => s.document_prompt.as_str(),
        };
        let inputs: Vec<String> = texts
            .iter()
            .map(|t| {
                let t = format!("{prompt}{t}");
                if s.lowercase { t.to_lowercase() } else { t }
            })
            .collect();
        let enc = self
            .tokenizer
            .encode_batch(inputs, true)
            .map_err(|e| Error::Model(format!("tokenizer: {e}")))?;
        let mut rows: Vec<(Vec<u32>, Vec<u32>)> = enc
            .iter()
            .map(|e| (e.get_ids().to_vec(), e.get_type_ids().to_vec()))
            .collect();
        if let Some(eos) = s.append_eos {
            for (ids, types) in &mut rows {
                if ids.last() != Some(&eos) {
                    ids.push(eos);
                    types.push(0);
                }
            }
        }
        let b = rows.len();
        let l = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max(1);
        let mut ids = vec![s.pad_id; b * l];
        let mut types = vec![0u32; b * l];
        let mut mask = vec![0u32; b * l];
        for (r, (ri, rt)) in rows.iter().enumerate() {
            let off = if s.left_padding { l - ri.len() } else { 0 };
            for (j, (&i, &t)) in ri.iter().zip(rt).enumerate() {
                ids[r * l + off + j] = i;
                types[r * l + off + j] = t;
                mask[r * l + off + j] = 1;
            }
        }
        let dev = Device::Cpu;
        let ids_t = Tensor::from_vec(ids, (b, l), &dev).map_err(cerr)?;
        let types_t = Tensor::from_vec(types, (b, l), &dev).map_err(cerr)?;
        let mask_t = Tensor::from_vec(mask.clone(), (b, l), &dev).map_err(cerr)?;
        let out = match &self.encoder {
            Encoder::Bert(m) => m.forward(&ids_t, &types_t, Some(&mask_t)),
            Encoder::XlmRoberta(m) => m.forward(&ids_t, &mask_t, &types_t, None, None, None),
            Encoder::Nomic(m) => m.forward(&ids_t, &types_t, &mask),
            Encoder::Qwen3(m) => m.forward(&ids_t, &mask),
        }
        .map_err(cerr)?;
        let (_, _, h) = out.dims3().map_err(cerr)?;
        let flat: Vec<f32> = out
            .to_dtype(DType::F32)
            .and_then(|t| t.flatten_all())
            .and_then(|t| t.to_vec1())
            .map_err(cerr)?;
        let mut vs = pool(s.pooling, &flat, &mask, b, l, h);
        for v in &mut vs {
            if s.normalize {
                normalize(v);
            }
            truncate(v, s.dimension, s.normalize);
        }
        Ok(vs)
    }
}
