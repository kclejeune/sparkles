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
        // kept transposed, `(in, out)`, so each product reads the weight row-major
        let w = vb.get((o, i), "weight")?.t()?.contiguous()?;
        let b = if bias {
            Some(vb.get(o, "bias")?.to_dtype(DType::F32)?)
        } else {
            None
        };
        Ok(Lin { w, b })
    }

    /// `x` `(..., in)` to `(..., out)`. The leading dimensions are folded into one, so
    /// the product is a single 2-D matmul against the transposed weight, which the CPU
    /// kernel reads through strides. A batched product would broadcast the weight and
    /// copy it for every call.
    pub(crate) fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let w = self.w.to_dtype(DType::F32)?;
        let dims = x.dims().to_vec();
        let (rows, i) = match dims.split_last() {
            Some((&i, lead)) => (lead.iter().product::<usize>(), i),
            None => candle_core::bail!("a linear layer needs at least one dimension"),
        };
        let y = x.reshape((rows, i))?.matmul(&w)?;
        let y = match &self.b {
            Some(b) => y.broadcast_add(b)?,
            None => y,
        };
        let mut out = dims;
        if let Some(last) = out.last_mut() {
            *last = y.dim(1)?;
        }
        let _ = i;
        y.reshape(out)
    }
}
