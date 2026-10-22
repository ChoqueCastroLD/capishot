//! TAESD (Tiny AutoEncoder for Stable Diffusion, madebyollin, MIT): a 2.4 M-parameter
//! encoder/decoder pair for the SD 1.x latent space, ~100× cheaper than the full VAE. Used for
//! variant previews (and the Fast tier); latents are the UNet's (already scaled) latents and
//! images are 0..1. Tensor names are TAESD's own `Sequential` indices, prefixed `encoder.` /
//! `decoder.` by the converter.

use burn::tensor::Tensor;
use burn::tensor::activation::{relu, tanh};
use burn::tensor::module::interpolate;
use burn::tensor::ops::{InterpolateMode, InterpolateOptions};

use crate::nn::{Conv, Store};
use crate::{LATENT_CHANNELS, Result, SdError};

const C: usize = 64;

#[derive(Debug, Clone)]
struct Block([Conv; 3]);

impl Block {
    fn load(s: &Store, p: &str) -> Result<Self> {
        Ok(Self([s.conv(&format!("{p}.conv.0"), C, C, 3)?, s.conv(&format!("{p}.conv.2"), C, C, 3)?, s.conv(&format!("{p}.conv.4"), C, C, 3)?]))
    }

    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        let [a, b, c] = &self.0;
        let h = c.forward(relu(b.forward(relu(a.forward(x.clone())))));
        relu(h + x)
    }
}

#[derive(Debug, Clone)]
enum Op {
    Conv(Conv),
    Stride2(Conv),
    Block(Box<Block>),
    Relu,
    Up,
}

fn run(ops: &[Op], mut x: Tensor<4>) -> Tensor<4> {
    for op in ops {
        x = match op {
            Op::Conv(c) => c.forward(x),
            Op::Stride2(c) => c.forward_stride2(x, 1),
            Op::Block(b) => b.forward(x),
            Op::Relu => relu(x),
            Op::Up => {
                let [_, _, h, w] = x.dims();
                interpolate(x, InterpolateOptions::new(InterpolateMode::Nearest).with_output_size([h * 2, w * 2]))
            }
        };
    }
    x
}

/// TAESD encoder: image 0..1 `[B, 3, H, W]` → scaled latent `[B, 4, H/8, W/8]`.
#[derive(Debug, Clone)]
pub struct TinyEncoder(Vec<Op>);

/// TAESD decoder: scaled latent → image 0..1 (not clamped).
#[derive(Debug, Clone)]
pub struct TinyDecoder(Vec<Op>);

impl TinyEncoder {
    pub fn from_store(s: &Store) -> Result<Self> {
        let p = |i: usize| format!("encoder.{i}");
        let mut ops = vec![Op::Conv(s.conv(&p(0), 3, C, 3)?), Op::Block(Box::new(Block::load(s, &p(1))?))];
        for base in [2, 6, 10] {
            ops.push(Op::Stride2(s.conv_nobias(&p(base), C, C, 3)?));
            for i in 1..=3 {
                ops.push(Op::Block(Box::new(Block::load(s, &p(base + i))?)));
            }
        }
        ops.push(Op::Conv(s.conv(&p(14), C, LATENT_CHANNELS, 3)?));
        Ok(Self(ops))
    }

    pub fn forward(&self, x: Tensor<4>) -> Result<Tensor<4>> {
        let [b, c, h, w] = x.dims();
        if c != 3 || h % 8 != 0 || w % 8 != 0 || h == 0 || w == 0 {
            return Err(SdError::Input(format!("TAESD encoder input {b}×{c}×{h}×{w}")));
        }
        Ok(run(&self.0, x))
    }
}

impl TinyDecoder {
    pub fn from_store(s: &Store) -> Result<Self> {
        let p = |i: usize| format!("decoder.{i}");
        let mut ops = vec![Op::Conv(s.conv(&p(1), LATENT_CHANNELS, C, 3)?), Op::Relu];
        for i in 3..=5 {
            ops.push(Op::Block(Box::new(Block::load(s, &p(i))?)));
        }
        for base in [6, 11] {
            ops.push(Op::Up);
            ops.push(Op::Conv(s.conv_nobias(&p(base + 1), C, C, 3)?));
            for i in 2..=4 {
                ops.push(Op::Block(Box::new(Block::load(s, &p(base + i))?)));
            }
        }
        ops.push(Op::Up);
        ops.push(Op::Conv(s.conv_nobias(&p(17), C, C, 3)?));
        ops.push(Op::Block(Box::new(Block::load(s, &p(18))?)));
        ops.push(Op::Conv(s.conv(&p(19), C, 3, 3)?));
        Ok(Self(ops))
    }

    pub fn forward(&self, z: Tensor<4>) -> Result<Tensor<4>> {
        let [b, c, h, w] = z.dims();
        if c != LATENT_CHANNELS || h == 0 || w == 0 {
            return Err(SdError::Input(format!("TAESD decoder input {b}×{c}×{h}×{w}")));
        }
        // Clamp: tanh(z/3)·3
        Ok(run(&self.0, tanh(z / 3.0) * 3.0))
    }
}
