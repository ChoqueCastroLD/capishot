//! Building blocks shared by the networks: the weight store (several safetensors shards, uploaded
//! tensor by tensor in the compute precision), linear/conv layers, group and layer norms, and
//! memory-bounded attention.
//!
//! Precision: weights and activations use the store's dtype (f16 on GPUs with `shader-f16`, f32
//! on the CPU and in reference runs); normalisation statistics and softmax are always computed
//! in f32.

use std::collections::HashMap;

use burn::tensor::activation::softmax;
use burn::tensor::module::conv2d;
use burn::tensor::ops::ConvOptions;
use burn::tensor::{DType, Device, Tensor, TensorCreationOptions, TensorData};
use safetensors::{Dtype, SafeTensors};

use crate::{Result, SdError};

/// Upper bound on one tensor's element count (the largest SD 1.5 tensor, the CLIP token
/// embedding, has 38 M): a hostile header can't ask for absurd allocations.
const MAX_TENSOR: usize = 64 << 20;

/// Tensors uploaded from one or more safetensors shards, by name.
#[derive(Debug)]
pub struct Store {
    device: Device,
    dtype: DType,
    map: HashMap<String, (Vec<usize>, Tensor<1>)>,
    metadata: HashMap<String, String>,
    bytes: u64,
}

impl Store {
    /// An empty store computing in `dtype` (`F16` or `F32`).
    pub fn new(device: &Device, dtype: DType) -> Result<Self> {
        if !matches!(dtype, DType::F16 | DType::F32) {
            return Err(SdError::Input(format!("unsupported compute precision {dtype:?}")));
        }
        Ok(Self { device: device.clone(), dtype, map: HashMap::new(), metadata: HashMap::new(), bytes: 0 })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn dtype(&self) -> DType {
        self.dtype
    }

    /// Bytes uploaded so far (in the compute precision).
    pub fn uploaded_bytes(&self) -> u64 {
        self.bytes
    }

    /// The `__metadata__` of the shards added so far (later shards win).
    pub fn metadata(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(String::as_str)
    }

    /// Uploads every floating-point tensor of a shard (other dtypes, e.g. the tokenizer's merge
    /// list, are skipped; read them with [`raw_tensor`]). The shard's bytes can be dropped
    /// afterwards: nothing borrows them.
    pub fn add(&mut self, shard: &[u8]) -> Result<()> {
        let st = SafeTensors::deserialize(shard).map_err(|e| SdError::Weights(format!("not a safetensors file: {e}")))?;
        let (_, header) = SafeTensors::read_metadata(shard).map_err(|e| SdError::Weights(format!("not a safetensors file: {e}")))?;
        if let Some(m) = header.metadata() {
            self.metadata.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        for (name, view) in st.tensors() {
            let dtype = match view.dtype() {
                Dtype::F16 => "F16",
                Dtype::BF16 => "BF16",
                Dtype::F32 => "F32",
                _ => continue,
            };
            self.upload(name, dtype, view.shape().to_vec(), view.data())?;
        }
        Ok(())
    }

    /// Uploads one tensor streamed by [`crate::stream::StreamParser`] (non-float tensors are
    /// skipped).
    pub fn add_raw(&mut self, t: &crate::stream::RawTensor) -> Result<()> {
        if !matches!(t.dtype.as_str(), "F16" | "BF16" | "F32") {
            return Ok(());
        }
        self.upload(t.name.clone(), &t.dtype, t.shape.clone(), &t.data)
    }

    /// Merges `__metadata__` entries (from a streamed header).
    pub fn add_metadata(&mut self, m: &HashMap<String, String>) {
        self.metadata.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
    }

    fn upload(&mut self, name: String, dtype: &str, shape: Vec<usize>, data: &[u8]) -> Result<()> {
        let n = shape.iter().try_fold(1usize, |a, &d| a.checked_mul(d)).filter(|&n| n > 0 && n <= MAX_TENSOR);
        let Some(n) = n else {
            return Err(SdError::Weights(format!("`{name}` has unsupported shape {shape:?}")));
        };
        let td = match (dtype, self.dtype) {
            ("F16", DType::F16) if data.len() == n * 2 => {
                // Reject inf/NaN (exponent all ones) without decoding.
                if data.as_chunks::<2>().0.iter().any(|b| u16::from_le_bytes(*b) & 0x7C00 == 0x7C00) {
                    return Err(SdError::Weights(format!("`{name}` has non-finite values")));
                }
                TensorData::from_bytes_vec(data.to_vec(), [n], DType::F16)
            }
            _ => {
                let st = match dtype {
                    "F16" => Dtype::F16,
                    "BF16" => Dtype::BF16,
                    _ => Dtype::F32,
                };
                let v = floats(&name, st, data, n)?;
                let td = TensorData::new(v, [n]);
                if self.dtype == DType::F16 { td.convert_dtype(DType::F16) } else { td }
            }
        };
        let t = Tensor::<1>::from_data(td, TensorCreationOptions::new(self.device.clone()).with_dtype(self.dtype));
        self.bytes = self.bytes.saturating_add((n * if self.dtype == DType::F16 { 2 } else { 4 }) as u64);
        self.map.insert(name, (shape, t));
        Ok(())
    }

    /// Takes over every tensor of `other` (shards of one network).
    pub fn absorb(&mut self, other: Store) {
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.metadata.extend(other.metadata);
        self.map.extend(other.map);
    }

    /// Tensor names (unordered).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    /// Whether a tensor is present.
    pub fn has(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    /// The shape of a tensor, if present.
    pub fn shape(&self, name: &str) -> Option<&[usize]> {
        self.map.get(name).map(|(s, _)| s.as_slice())
    }

    /// A tensor with exactly this shape.
    pub fn get<const D: usize>(&self, name: &str, shape: [usize; D]) -> Result<Tensor<D>> {
        let (s, t) = self.map.get(name).ok_or_else(|| SdError::Weights(format!("missing tensor `{name}`")))?;
        if s.as_slice() != shape.as_slice() {
            return Err(SdError::Weights(format!("`{name}` has shape {s:?}, expected {shape:?}")));
        }
        Ok(t.clone().reshape(shape))
    }

    /// A tensor flattened to one dimension, whatever its shape.
    pub fn get_flat(&self, name: &str) -> Result<Tensor<1>> {
        self.map.get(name).map(|(_, t)| t.clone()).ok_or_else(|| SdError::Weights(format!("missing tensor `{name}`")))
    }

    /// Replaces a tensor (LoRA fusion), keeping its shape.
    pub fn replace<const D: usize>(&mut self, name: &str, t: Tensor<D>) -> Result<()> {
        let entry = self.map.get_mut(name).ok_or_else(|| SdError::Weights(format!("missing tensor `{name}`")))?;
        if entry.0.as_slice() != t.dims().as_slice() {
            return Err(SdError::Weights(format!("`{name}`: replacement shape {:?} vs {:?}", t.dims(), entry.0)));
        }
        let n = t.dims().iter().product::<usize>();
        entry.1 = t.reshape([n]);
        Ok(())
    }

    /// Replaces a tensor with the same number of elements, given flat.
    pub fn replace_flat(&mut self, name: &str, t: Tensor<1>) -> Result<()> {
        let entry = self.map.get_mut(name).ok_or_else(|| SdError::Weights(format!("missing tensor `{name}`")))?;
        if entry.0.iter().product::<usize>() != t.dims()[0] {
            return Err(SdError::Weights(format!("`{name}`: replacement has {} values", t.dims()[0])));
        }
        entry.1 = t;
        Ok(())
    }

    /// Drops tensors whose name starts with `prefix` (frees device memory once a network has
    /// taken its weights).
    pub fn remove_prefix(&mut self, prefix: &str) {
        self.map.retain(|k, _| !k.starts_with(prefix));
    }

    pub fn linear(&self, name: &str, d_in: usize, d_out: usize, bias: bool) -> Result<Linear> {
        // Stored PyTorch-style [out, in]; burn's matmul wants [in, out].
        let w = self.get(&format!("{name}.weight"), [d_out, d_in])?.transpose();
        let b = if bias { Some(self.get(&format!("{name}.bias"), [d_out])?) } else { None };
        Ok(Linear { w, b })
    }

    pub fn conv(&self, name: &str, c_in: usize, c_out: usize, k: usize) -> Result<Conv> {
        let w = self.get(&format!("{name}.weight"), [c_out, c_in, k, k])?;
        let b = Some(self.get(&format!("{name}.bias"), [c_out])?);
        Ok(Conv { w, b, pad: k / 2 })
    }

    /// A convolution without bias.
    pub fn conv_nobias(&self, name: &str, c_in: usize, c_out: usize, k: usize) -> Result<Conv> {
        let w = self.get(&format!("{name}.weight"), [c_out, c_in, k, k])?;
        Ok(Conv { w, b: None, pad: k / 2 })
    }

    pub fn norm(&self, name: &str, c: usize) -> Result<Norm> {
        Ok(Norm { w: self.get(&format!("{name}.weight"), [c])?, b: self.get(&format!("{name}.bias"), [c])? })
    }

    /// A tensor of the compute precision from f32 values.
    pub fn tensor<const D: usize>(&self, v: Vec<f32>, shape: [usize; D]) -> Result<Tensor<D>> {
        let n = shape.iter().product::<usize>();
        if v.len() != n {
            return Err(SdError::Input(format!("{} values for shape {shape:?}", v.len())));
        }
        let td = TensorData::new(v, shape);
        let td = if self.dtype == DType::F16 { td.convert_dtype(DType::F16) } else { td };
        Ok(Tensor::from_data(td, TensorCreationOptions::new(self.device.clone()).with_dtype(self.dtype)))
    }
}

/// The raw bytes of a non-float tensor (e.g. `tokenizer.merges`, U8) in a shard.
pub fn raw_tensor(shard: &[u8], name: &str) -> Result<Vec<u8>> {
    let st = SafeTensors::deserialize(shard).map_err(|e| SdError::Weights(format!("not a safetensors file: {e}")))?;
    let t = st.tensor(name).map_err(|_| SdError::Weights(format!("missing tensor `{name}`")))?;
    Ok(t.data().to_vec())
}

fn floats(name: &str, dtype: Dtype, data: &[u8], n: usize) -> Result<Vec<f32>> {
    let v: Vec<f32> = match dtype {
        Dtype::F32 if data.len() == n * 4 => data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
        Dtype::F16 if data.len() == n * 2 => data.as_chunks::<2>().0.iter().map(|b| half::f16::from_le_bytes(*b).to_f32()).collect(),
        Dtype::BF16 if data.len() == n * 2 => data.as_chunks::<2>().0.iter().map(|b| half::bf16::from_le_bytes(*b).to_f32()).collect(),
        other => return Err(SdError::Weights(format!("`{name}`: unsupported dtype {other:?} or wrong byte length"))),
    };
    if v.iter().any(|x| !x.is_finite()) {
        return Err(SdError::Weights(format!("`{name}` has non-finite values")));
    }
    Ok(v)
}

/// `x·W + b` over the last dimension.
#[derive(Debug, Clone)]
pub struct Linear {
    pub w: Tensor<2>,
    pub b: Option<Tensor<1>>,
}

impl Linear {
    pub fn forward<const D: usize>(&self, x: Tensor<D>) -> Tensor<D> {
        burn::tensor::module::linear(x, self.w.clone(), self.b.clone())
    }
}

/// A square convolution, stride 1, "same" zero padding.
#[derive(Debug, Clone)]
pub struct Conv {
    pub w: Tensor<4>,
    pub b: Option<Tensor<1>>,
    pub pad: usize,
}

impl Conv {
    pub fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        conv2d(x, self.w.clone(), self.b.clone(), ConvOptions::new([1, 1], [self.pad, self.pad], [1, 1], 1))
    }

    /// Stride 2 with the given padding (UNet: 1 on every side).
    pub fn forward_stride2(&self, x: Tensor<4>, pad: usize) -> Tensor<4> {
        conv2d(x, self.w.clone(), self.b.clone(), ConvOptions::new([2, 2], [pad, pad], [1, 1], 1))
    }
}

/// Affine parameters of a group or layer norm.
#[derive(Debug, Clone)]
pub struct Norm {
    pub w: Tensor<1>,
    pub b: Tensor<1>,
}

impl Norm {
    /// GroupNorm over `[B, C, H, W]` with `groups` groups; statistics in f32.
    pub fn group(&self, x: Tensor<4>, groups: usize, eps: f64) -> Tensor<4> {
        let dt = x.dtype();
        let [b, c, h, w] = x.dims();
        let x = x.cast(DType::F32).reshape([b, groups, (c / groups) * h * w]);
        let mean = x.clone().mean_dim(2);
        let xc = x - mean;
        let var = xc.clone().square().mean_dim(2);
        let x = (xc / (var + eps).sqrt()).reshape([b, c, h, w]).cast(dt);
        x * self.w.clone().reshape([1, c, 1, 1]) + self.b.clone().reshape([1, c, 1, 1])
    }

    /// LayerNorm over the last dimension; statistics in f32.
    pub fn layer<const D: usize>(&self, x: Tensor<D>, eps: f64) -> Tensor<D> {
        let dt = x.dtype();
        let x = x.cast(DType::F32);
        let mean = x.clone().mean_dim(D - 1);
        let xc = x - mean;
        let var = xc.clone().square().mean_dim(D - 1);
        let x = (xc / (var + eps).sqrt()).cast(dt);
        let n = self.w.dims()[0];
        let mut shape = [1usize; D];
        shape[D - 1] = n;
        x * self.w.clone().reshape(shape) + self.b.clone().reshape(shape)
    }
}

/// Bytes of attention scores allowed at once (f32): bounds the transient memory of a 64×64
/// latent's self-attention (4096² per head) by chunking the queries.
pub const ATTENTION_BUDGET: usize = 128 << 20;

/// Multi-head attention `softmax(q·kᵀ·scale + mask)·v` on `[B, H, N, d]` tensors, with the
/// queries processed in chunks so the f32 score matrix stays under [`ATTENTION_BUDGET`].
/// `causal` masks keys after the query (CLIP).
pub fn attention(q: Tensor<4>, k: Tensor<4>, v: Tensor<4>, causal: bool) -> Tensor<4> {
    let [b, h, n, d] = q.dims();
    let m = k.dims()[2];
    let dt = q.dtype();
    let scale = 1.0 / (d as f64).sqrt();
    let kt = k.swap_dims(2, 3);
    let per_row = b * h * m * 4;
    let chunk = (ATTENTION_BUDGET / per_row.max(1)).clamp(1, n.max(1));
    let mut outs = Vec::with_capacity(n.div_ceil(chunk));
    let mut start = 0;
    while start < n {
        let end = (start + chunk).min(n);
        let qc = q.clone().slice([0..b, 0..h, start..end, 0..d]);
        let mut s = qc.matmul(kt.clone()).cast(DType::F32) * scale;
        if causal {
            let rows = end - start;
            let mut bias = vec![0f32; rows * m];
            for r in 0..rows {
                for c in (start + r + 1)..m {
                    if let Some(x) = bias.get_mut(r * m + c) {
                        *x = f32::NEG_INFINITY;
                    }
                }
            }
            let bias = Tensor::<2>::from_data(TensorData::new(bias, [rows, m]), TensorCreationOptions::new(s.device()).with_dtype(DType::F32));
            s = s + bias.reshape([1, 1, rows, m]);
        }
        let p = softmax(s, 3).cast(dt);
        outs.push(p.matmul(v.clone()));
        start = end;
    }
    if outs.len() == 1 { outs.remove(0) } else { Tensor::cat(outs, 2) }
}

/// `[B, N, H·d]` → `[B, H, N, d]`.
pub fn split_heads(x: Tensor<3>, heads: usize) -> Tensor<4> {
    let [b, n, c] = x.dims();
    x.reshape([b, n, heads, c / heads]).swap_dims(1, 2)
}

/// `[B, H, N, d]` → `[B, N, H·d]`.
pub fn merge_heads(x: Tensor<4>) -> Tensor<3> {
    let [b, h, n, d] = x.dims();
    x.swap_dims(1, 2).reshape([b, n, h * d])
}
