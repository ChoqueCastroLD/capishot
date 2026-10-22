//! The CLIP ViT-L/14 text encoder (12 layers, width 768, 12 heads, quick-GELU, causal), as
//! Stable Diffusion 1.x uses it: the last hidden state after the final layer norm.

use burn::tensor::activation::sigmoid;
use burn::tensor::{Int, Tensor, TensorCreationOptions, TensorData};

use crate::nn::{Linear, Norm, Store, Weight, attention, merge_heads, split_heads};
use crate::tokenizer::CONTEXT;
use crate::{Result, SdError};

const WIDTH: usize = 768;
const LAYERS: usize = 12;
const HEADS: usize = 12;
const MLP: usize = 3072;
const VOCAB: usize = 49408;
const EPS: f64 = 1e-5;

#[derive(Debug, Clone)]
struct Layer {
    ln1: Norm,
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
    ln2: Norm,
    fc1: Linear,
    fc2: Linear,
}

/// The text encoder.
#[derive(Debug, Clone)]
pub struct TextEncoder {
    token: Weight<2>,
    position: Tensor<2>,
    layers: Vec<Layer>,
    final_ln: Norm,
}

impl TextEncoder {
    /// Takes the `text_model.*` tensors of `store`.
    pub fn from_store(s: &Store) -> Result<Self> {
        let p = "text_model";
        let token = s.weight(&format!("{p}.embeddings.token_embedding.weight"), [VOCAB, WIDTH])?;
        let position = s.get(&format!("{p}.embeddings.position_embedding.weight"), [CONTEXT, WIDTH])?;
        let mut layers = Vec::with_capacity(LAYERS);
        for i in 0..LAYERS {
            let l = format!("{p}.encoder.layers.{i}");
            layers.push(Layer {
                ln1: s.norm(&format!("{l}.layer_norm1"), WIDTH)?,
                q: s.linear(&format!("{l}.self_attn.q_proj"), WIDTH, WIDTH, true)?,
                k: s.linear(&format!("{l}.self_attn.k_proj"), WIDTH, WIDTH, true)?,
                v: s.linear(&format!("{l}.self_attn.v_proj"), WIDTH, WIDTH, true)?,
                out: s.linear(&format!("{l}.self_attn.out_proj"), WIDTH, WIDTH, true)?,
                ln2: s.norm(&format!("{l}.layer_norm2"), WIDTH)?,
                fc1: s.linear(&format!("{l}.mlp.fc1"), WIDTH, MLP, true)?,
                fc2: s.linear(&format!("{l}.mlp.fc2"), MLP, WIDTH, true)?,
            });
        }
        let final_ln = s.norm(&format!("{p}.final_layer_norm"), WIDTH)?;
        Ok(Self { token, position, layers, final_ln })
    }

    /// Encodes token ids (`[B][77]`, from [`crate::tokenizer::Tokenizer::encode_padded`]) to
    /// `[B, 77, 768]`.
    pub fn forward(&self, ids: &[[u32; CONTEXT]]) -> Result<Tensor<3>> {
        let b = ids.len();
        if b == 0 {
            return Err(SdError::Input("no prompts".into()));
        }
        let flat: Vec<i32> = ids.iter().flat_map(|r| r.iter().map(|&t| i32::try_from(t).unwrap_or(0).clamp(0, VOCAB as i32 - 1))).collect();
        let device = self.position.device();
        let idx = Tensor::<1, Int>::from_data(TensorData::new(flat, [b * CONTEXT]), TensorCreationOptions::new(device));
        // A quantized table stays quantized: only the prompt's rows are dequantized.
        let rows = match &self.token {
            Weight::Dense(t) => t.clone().select(0, idx),
            Weight::Quant(q, _, dt) => q.clone().select(0, idx).dequantize().cast(*dt),
        };
        let x = rows.reshape([b, CONTEXT, WIDTH]);
        let mut x = x + self.position.clone().reshape([1, CONTEXT, WIDTH]);
        for l in &self.layers {
            let h = l.ln1.layer(x.clone(), EPS);
            let q = split_heads(l.q.forward(h.clone()), HEADS);
            let k = split_heads(l.k.forward(h.clone()), HEADS);
            let v = split_heads(l.v.forward(h), HEADS);
            x = x + l.out.forward(merge_heads(attention(q, k, v, true)));
            let h = l.fc1.forward(l.ln2.layer(x.clone(), EPS));
            // quick_gelu: x·σ(1.702·x)
            let h = h.clone() * sigmoid(h * 1.702);
            x = x + l.fc2.forward(h);
        }
        Ok(self.final_ln.layer(x, EPS))
    }
}
