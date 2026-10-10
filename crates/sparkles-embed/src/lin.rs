//! The linear layer the encoders of this crate share.

use candle_core::{DType, Result, Tensor};
use candle_nn::VarBuilder;

/// A linear layer whose weight is stored at the model's type and computed in f32: Candle's
/// CPU matmul has no bf16 kernel, so a bf16 weight is widened for each product. The
/// weights stay half the size in memory at the cost of that conversion.
pub(crate) struct Lin {
    w: Tensor,
    b: Option<Tensor>,
}

impl Lin {
    pub(crate) fn load(i: usize, o: usize, bias: bool, vb: VarBuilder) -> Result<Lin> {
        let w = vb.get((o, i), "weight")?;
        let b = if bias {
            Some(vb.get(o, "bias")?.to_dtype(DType::F32)?)
        } else {
            None
        };
        Ok(Lin { w, b })
    }

    pub(crate) fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let w = self.w.to_dtype(DType::F32)?;
        let y = x.broadcast_matmul(&w.t()?)?;
        match &self.b {
            Some(b) => y.broadcast_add(b),
            None => Ok(y),
        }
    }
}
