//! The NomicBERT encoder (`model_type` `nomic_bert`, as in nomic-embed-text): BERT
//! with rotary position embeddings instead of learned ones, a fused query-key-value
//! projection, a SwiGLU MLP and, by default, no biases and post-norm layers.
//!
//! The field names follow the model's `config.json`. Weights may sit at the top level or
//! under the `nomic_bert.` prefix of some checkpoints. Norms run in f32 and the linear
//! layers widen bf16 weights for each product, so bf16 weights work.

use candle_core::{D, DType, Device, Module, Result, Tensor};
use candle_nn::{Embedding, LayerNorm, VarBuilder, embedding, layer_norm};
use serde::Deserialize;

use crate::lin::Lin;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub vocab_size: usize,
    pub n_embd: usize,
    pub n_head: usize,
    pub n_layer: usize,
    pub n_inner: usize,
    #[serde(default = "default_type_vocab")]
    pub type_vocab_size: usize,
    #[serde(default = "default_eps")]
    pub layer_norm_epsilon: f64,
    #[serde(default = "default_fraction")]
    pub rotary_emb_fraction: f64,
    #[serde(default = "default_base")]
    pub rotary_emb_base: f64,
    #[serde(default)]
    pub rotary_emb_interleaved: bool,
    #[serde(default)]
    pub rotary_scaling_factor: Option<f64>,
    #[serde(default)]
    pub qkv_proj_bias: bool,
    #[serde(default)]
    pub mlp_fc1_bias: bool,
    #[serde(default)]
    pub mlp_fc2_bias: bool,
    #[serde(default = "default_activation")]
    pub activation_function: String,
    #[serde(default)]
    pub prenorm: bool,
    #[serde(default)]
    pub parallel_block: bool,
    #[serde(default)]
    pub use_rms_norm: bool,
}

fn default_type_vocab() -> usize {
    2
}

fn default_eps() -> f64 {
    1e-12
}

fn default_fraction() -> f64 {
    1.0
}

fn default_base() -> f64 {
    10_000.0
}

fn default_activation() -> String {
    "swiglu".into()
}

struct Block {
    wqkv: Lin,
    out: Lin,
    fc11: Lin,
    fc12: Lin,
    fc2: Lin,
    norm1: LayerNorm,
    norm2: LayerNorm,
}

pub struct NomicEncoder {
    words: Embedding,
    types: Option<Embedding>,
    emb_ln: LayerNorm,
    blocks: Vec<Block>,
    heads: usize,
    head_dim: usize,
    rot_dim: usize,
    base: f64,
    prenorm: bool,
    device: Device,
}

impl NomicEncoder {
    pub fn load(cfg: &Config, vb: VarBuilder) -> Result<Self> {
        if cfg.activation_function != "swiglu" {
            candle_core::bail!(
                "nomic_bert: unsupported activation_function {:?}",
                cfg.activation_function
            );
        }
        if cfg.rotary_emb_interleaved
            || cfg.rotary_scaling_factor.is_some()
            || cfg.parallel_block
            || cfg.use_rms_norm
        {
            candle_core::bail!(
                "nomic_bert: interleaved or scaled rotary embeddings, parallel blocks and RMS norms are not supported"
            );
        }
        if cfg.n_head == 0 || !cfg.n_embd.is_multiple_of(cfg.n_head) {
            candle_core::bail!("nomic_bert: n_embd is not a multiple of n_head");
        }
        let head_dim = cfg.n_embd / cfg.n_head;
        let rot_dim = ((head_dim as f64 * cfg.rotary_emb_fraction) as usize) & !1;
        if rot_dim == 0 || rot_dim > head_dim {
            candle_core::bail!("nomic_bert: rotary_emb_fraction leaves no rotated dimension");
        }
        let vb = if vb.contains_tensor("nomic_bert.emb_ln.weight") {
            vb.pp("nomic_bert")
        } else {
            vb
        };
        let vf = vb.clone().set_dtype(DType::F32);
        let h = cfg.n_embd;
        let eps = cfg.layer_norm_epsilon;
        let e = vb.pp("embeddings");
        let words = embedding(cfg.vocab_size, h, e.pp("word_embeddings"))?;
        let types = if cfg.type_vocab_size > 0 {
            Some(embedding(
                cfg.type_vocab_size,
                h,
                e.pp("token_type_embeddings"),
            )?)
        } else {
            None
        };
        let mut blocks = Vec::with_capacity(cfg.n_layer);
        for i in 0..cfg.n_layer {
            let l = vb.pp("encoder.layers").pp(i);
            let lf = vf.pp("encoder.layers").pp(i);
            let (a, m) = (l.pp("attn"), l.pp("mlp"));
            blocks.push(Block {
                wqkv: Lin::load(h, 3 * h, cfg.qkv_proj_bias, a.pp("Wqkv"))?,
                out: Lin::load(h, h, cfg.qkv_proj_bias, a.pp("out_proj"))?,
                fc11: Lin::load(h, cfg.n_inner, cfg.mlp_fc1_bias, m.pp("fc11"))?,
                fc12: Lin::load(h, cfg.n_inner, cfg.mlp_fc1_bias, m.pp("fc12"))?,
                fc2: Lin::load(cfg.n_inner, h, cfg.mlp_fc2_bias, m.pp("fc2"))?,
                norm1: layer_norm(h, eps, lf.pp("norm1"))?,
                norm2: layer_norm(h, eps, lf.pp("norm2"))?,
            });
        }
        Ok(NomicEncoder {
            words,
            types,
            emb_ln: layer_norm(h, eps, vf.pp("emb_ln"))?,
            blocks,
            heads: cfg.n_head,
            head_dim,
            rot_dim,
            base: cfg.rotary_emb_base,
            prenorm: cfg.prenorm,
            device: vb.device().clone(),
        })
    }

    /// Rotary tables for positions `0..len`, shape `(len, rot_dim / 2)`.
    fn rope_tables(&self, len: usize) -> Result<(Tensor, Tensor)> {
        let half = self.rot_dim / 2;
        let inv: Vec<f32> = (0..half)
            .map(|i| 1.0 / (self.base as f32).powf(2.0 * i as f32 / self.rot_dim as f32))
            .collect();
        let inv = Tensor::from_vec(inv, (1, half), &self.device)?;
        let t = Tensor::arange(0u32, len as u32, &self.device)?
            .to_dtype(DType::F32)?
            .reshape((len, 1))?;
        let f = t.matmul(&inv)?;
        Ok((f.cos()?, f.sin()?))
    }

    /// Rotate the first `rot_dim` components of each head of `x` `(b, heads, len, hd)`.
    fn rope(&self, x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
        if self.rot_dim == self.head_dim {
            return candle_nn::rotary_emb::rope(&x.contiguous()?, cos, sin);
        }
        let r = x.narrow(D::Minus1, 0, self.rot_dim)?.contiguous()?;
        let rest = x.narrow(D::Minus1, self.rot_dim, self.head_dim - self.rot_dim)?;
        Tensor::cat(
            &[candle_nn::rotary_emb::rope(&r, cos, sin)?, rest],
            D::Minus1,
        )
    }

    /// Last-layer hidden states `(batch, len, hidden)`. Padding keys are hidden from
    /// every query, and each text has at least its special tokens, so no row of the
    /// softmax is empty.
    pub fn forward(&self, ids: &Tensor, types: &Tensor, mask_host: &[u32]) -> Result<Tensor> {
        let (b, l) = ids.dims2()?;
        let mut x = self.words.forward(ids)?.to_dtype(DType::F32)?;
        if let Some(t) = &self.types {
            x = (x + t.forward(types)?.to_dtype(DType::F32)?)?;
        }
        let mut h = self.emb_ln.forward(&x)?;
        let bias: Vec<f32> = mask_host
            .iter()
            .map(|&m| if m == 0 { -1e9 } else { 0.0 })
            .collect();
        let bias = Tensor::from_vec(bias, (b, 1, 1, l), &self.device)?;
        let (cos, sin) = self.rope_tables(l)?;
        let scale = 1.0 / (self.head_dim as f64).sqrt();
        let n = self.heads * self.head_dim;
        for blk in &self.blocks {
            let input = if self.prenorm {
                blk.norm1.forward(&h)?
            } else {
                h.clone()
            };
            let qkv = blk.wqkv.forward(&input)?;
            let split = |i: usize| -> Result<Tensor> {
                qkv.narrow(D::Minus1, i * n, n)?
                    .reshape((b, l, self.heads, self.head_dim))?
                    .transpose(1, 2)
            };
            let q = self.rope(&split(0)?, &cos, &sin)?;
            let k = self.rope(&split(1)?, &cos, &sin)?;
            let v = split(2)?.contiguous()?;
            let scores = (q.matmul(&k.t()?)? * scale)?.broadcast_add(&bias)?;
            let probs = candle_nn::ops::softmax_last_dim(&scores)?;
            let ctx = probs.matmul(&v)?.transpose(1, 2)?.reshape((b, l, n))?;
            let attn = blk.out.forward(&ctx)?;
            if self.prenorm {
                h = (h + attn)?;
                let y = blk.norm2.forward(&h)?;
                let mlp = blk.fc2.forward(
                    &(blk.fc11.forward(&y)? * candle_nn::ops::silu(&blk.fc12.forward(&y)?)?)?,
                )?;
                h = (h + mlp)?;
            } else {
                let y = blk.norm1.forward(&(h + attn)?)?;
                let mlp = blk.fc2.forward(
                    &(blk.fc11.forward(&y)? * candle_nn::ops::silu(&blk.fc12.forward(&y)?)?)?,
                )?;
                h = blk.norm2.forward(&(y + mlp)?)?;
            }
        }
        Ok(h)
    }
}
