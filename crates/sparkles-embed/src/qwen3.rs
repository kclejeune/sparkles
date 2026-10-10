//! The Qwen3 decoder as a text encoder: the hidden states of the last layer for a padded
//! batch, under a causal mask that also hides padding.
//!
//! Candle's own `qwen3::Model` keeps a key-value cache across calls and masks only
//! causally, so a left-padded batch would let real tokens attend to padding. This
//! version is stateless and takes the padding mask. The layer structure follows the
//! Qwen3 configuration of Hugging Face Transformers: RMSNorm before attention and MLP,
//! per-head RMSNorm of queries and keys, rotary embeddings (half rotation), grouped
//! query attention and a SwiGLU MLP.

use candle_core::{DType, Device, Module, Result, Tensor};
use candle_nn::{Embedding, RmsNorm, VarBuilder, embedding, rms_norm};

use crate::lin::Lin;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub vocab_size: usize,
    pub hidden_size: usize,
    pub intermediate_size: usize,
    pub num_hidden_layers: usize,
    pub num_attention_heads: usize,
    pub num_key_value_heads: usize,
    #[serde(default)]
    pub head_dim: Option<usize>,
    #[serde(default)]
    pub attention_bias: bool,
    #[serde(default = "default_theta")]
    pub rope_theta: f64,
    #[serde(default = "default_eps")]
    pub rms_norm_eps: f64,
    #[serde(default)]
    pub hidden_act: Option<String>,
}

fn default_theta() -> f64 {
    1_000_000.0
}

fn default_eps() -> f64 {
    1e-6
}

impl Config {
    fn head_dim(&self) -> usize {
        self.head_dim
            .unwrap_or(self.hidden_size / self.num_attention_heads)
    }
}

struct Layer {
    ln1: RmsNorm,
    ln2: RmsNorm,
    q: Lin,
    k: Lin,
    v: Lin,
    o: Lin,
    q_norm: RmsNorm,
    k_norm: RmsNorm,
    gate: Lin,
    up: Lin,
    down: Lin,
}

pub struct Qwen3Encoder {
    embed: Embedding,
    layers: Vec<Layer>,
    norm: RmsNorm,
    heads: usize,
    kv_heads: usize,
    head_dim: usize,
    theta: f64,
    device: Device,
}

impl Qwen3Encoder {
    pub fn load(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        if let Some(a) = &cfg.hidden_act
            && a != "silu"
        {
            candle_core::bail!("qwen3: unsupported hidden_act {a:?}");
        }
        if cfg.num_key_value_heads == 0
            || !cfg
                .num_attention_heads
                .is_multiple_of(cfg.num_key_value_heads)
        {
            candle_core::bail!("qwen3: attention heads are not a multiple of key-value heads");
        }
        // Qwen3-Embedding stores the base model without the `model.` prefix of the
        // causal-LM checkpoints; accept both.
        let vb = if vb.contains_tensor("model.embed_tokens.weight") {
            vb.pp("model")
        } else {
            vb
        };
        let hd = cfg.head_dim();
        let vf = vb.clone().set_dtype(DType::F32);
        let embed = embedding(cfg.vocab_size, cfg.hidden_size, vb.pp("embed_tokens"))?;
        let mut layers = Vec::with_capacity(cfg.num_hidden_layers);
        for i in 0..cfg.num_hidden_layers {
            let l = vb.pp("layers").pp(i);
            let lf = vf.pp("layers").pp(i);
            let a = l.pp("self_attn");
            let af = lf.pp("self_attn");
            let m = l.pp("mlp");
            let h = cfg.hidden_size;
            layers.push(Layer {
                ln1: rms_norm(h, cfg.rms_norm_eps, lf.pp("input_layernorm"))?,
                ln2: rms_norm(h, cfg.rms_norm_eps, lf.pp("post_attention_layernorm"))?,
                q: Lin::load(
                    h,
                    cfg.num_attention_heads * hd,
                    cfg.attention_bias,
                    a.pp("q_proj"),
                )?,
                k: Lin::load(
                    h,
                    cfg.num_key_value_heads * hd,
                    cfg.attention_bias,
                    a.pp("k_proj"),
                )?,
                v: Lin::load(
                    h,
                    cfg.num_key_value_heads * hd,
                    cfg.attention_bias,
                    a.pp("v_proj"),
                )?,
                o: Lin::load(
                    cfg.num_attention_heads * hd,
                    h,
                    cfg.attention_bias,
                    a.pp("o_proj"),
                )?,
                q_norm: rms_norm(hd, cfg.rms_norm_eps, af.pp("q_norm"))?,
                k_norm: rms_norm(hd, cfg.rms_norm_eps, af.pp("k_norm"))?,
                gate: Lin::load(h, cfg.intermediate_size, false, m.pp("gate_proj"))?,
                up: Lin::load(h, cfg.intermediate_size, false, m.pp("up_proj"))?,
                down: Lin::load(cfg.intermediate_size, h, false, m.pp("down_proj"))?,
            });
        }
        Ok(Qwen3Encoder {
            embed,
            layers,
            norm: rms_norm(cfg.hidden_size, cfg.rms_norm_eps, vf.pp("norm"))?,
            heads: cfg.num_attention_heads,
            kv_heads: cfg.num_key_value_heads,
            head_dim: hd,
            theta: cfg.rope_theta,
            device: vb.device().clone(),
        })
    }

    /// Rotary tables for positions `0..len`, shape `(len, head_dim / 2)`.
    fn rope_tables(&self, len: usize) -> Result<(Tensor, Tensor)> {
        let half = self.head_dim / 2;
        let inv: Vec<f32> = (0..half)
            .map(|i| 1.0 / self.theta.powf(2.0 * i as f64 / self.head_dim as f64) as f32)
            .collect();
        let inv = Tensor::from_vec(inv, (1, half), &self.device)?;
        let t = Tensor::arange(0u32, len as u32, &self.device)?
            .to_dtype(DType::F32)?
            .reshape((len, 1))?;
        let f = t.matmul(&inv)?;
        Ok((f.cos()?, f.sin()?))
    }

    /// The additive attention mask `(batch, 1, len, len)` in f32: a query sees the keys at
    /// or before its position that are not padding. A padding query sees itself only, so
    /// its row of the softmax is defined and no NaN reaches later layers.
    fn mask(&self, mask: &[u32], b: usize, l: usize) -> Result<Tensor> {
        let mut m = vec![0f32; b * l * l];
        for r in 0..b {
            let row = &mask[r * l..(r + 1) * l];
            for i in 0..l {
                for j in 0..l {
                    let ok = j == i || (j < i && row[j] != 0);
                    if !ok {
                        m[(r * l + i) * l + j] = f32::NEG_INFINITY;
                    }
                }
            }
        }
        Tensor::from_vec(m, (b, 1, l, l), &self.device)
    }

    /// Last-layer hidden states `(batch, len, hidden)` of `ids` `(batch, len)`.
    pub fn forward(&self, ids: &Tensor, mask_host: &[u32]) -> Result<Tensor> {
        let (b, l) = ids.dims2()?;
        let (cos, sin) = self.rope_tables(l)?;
        let mask = self.mask(mask_host, b, l)?;
        let scale = 1.0 / (self.head_dim as f64).sqrt();
        let groups = self.heads / self.kv_heads;
        let mut h = self.embed.forward(ids)?.to_dtype(DType::F32)?;
        for layer in &self.layers {
            let x = layer.ln1.forward(&h)?;
            let q = layer
                .q
                .forward(&x)?
                .reshape((b, l, self.heads, self.head_dim))?
                .transpose(1, 2)?
                .contiguous()?;
            let k = layer
                .k
                .forward(&x)?
                .reshape((b, l, self.kv_heads, self.head_dim))?
                .transpose(1, 2)?
                .contiguous()?;
            let v = layer
                .v
                .forward(&x)?
                .reshape((b, l, self.kv_heads, self.head_dim))?
                .transpose(1, 2)?;
            let q = layer.q_norm.forward(&q)?;
            let k = layer.k_norm.forward(&k)?;
            let q = candle_nn::rotary_emb::rope(&q.contiguous()?, &cos, &sin)?;
            let k = candle_nn::rotary_emb::rope(&k.contiguous()?, &cos, &sin)?;
            let k = candle_transformers::utils::repeat_kv(k, groups)?.contiguous()?;
            let v = candle_transformers::utils::repeat_kv(v, groups)?.contiguous()?;
            let scores = (q.matmul(&k.t()?)? * scale)?.broadcast_add(&mask)?;
            let probs = candle_nn::ops::softmax_last_dim(&scores)?;
            let ctx =
                probs
                    .matmul(&v)?
                    .transpose(1, 2)?
                    .reshape((b, l, self.heads * self.head_dim))?;
            h = (h + layer.o.forward(&ctx)?)?;
            let x = layer.ln2.forward(&h)?;
            let mlp = layer.down.forward(
                &(candle_nn::ops::silu(&layer.gate.forward(&x)?)? * layer.up.forward(&x)?)?,
            )?;
            h = (h + mlp)?;
        }
        self.norm.forward(&h)
    }
}
