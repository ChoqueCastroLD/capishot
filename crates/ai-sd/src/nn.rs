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
    /// Parts of block-quantized tensors still waiting for their companions.
    pending: HashMap<String, QuantParts>,
    /// Block-quantized weights as `[out, fan_in]` (blocks along `fan_in`), with their shapes;
    /// dequantized per layer at use ([`Weight`]).
    quant: HashMap<String, (Vec<usize>, Tensor<2>)>,
}

/// A block-quantized tensor as the quantizer writes it: `name.q4` (two signed 4-bit values per
/// byte, low nibble first) or `name.q8` (int8), `name.qs` (one f16 scale per [`QUANT_BLOCK`]
/// values), `name.qshape` (i32 shape).
#[derive(Debug, Default)]
struct QuantParts {
    values: Option<(u8, Vec<u8>)>,
    scales: Option<Vec<f32>>,
    shape: Option<Vec<usize>>,
}

/// Values per quantization scale.
pub const QUANT_BLOCK: usize = 32;

impl Store {
    /// An empty store computing in `dtype` (`F16` or `F32`).
    pub fn new(device: &Device, dtype: DType) -> Result<Self> {
        if !matches!(dtype, DType::F16 | DType::F32) {
            return Err(SdError::Input(format!("unsupported compute precision {dtype:?}")));
        }
        Ok(Self { device: device.clone(), dtype, map: HashMap::new(), metadata: HashMap::new(), bytes: 0, pending: HashMap::new(), quant: HashMap::new() })
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
                Dtype::I8 => "I8",
                Dtype::U8 => "U8",
                Dtype::I32 => "I32",
                _ => continue,
            };
            self.ingest(name, dtype, view.shape().to_vec(), view.data())?;
        }
        self.check_complete()
    }

    /// Fails when a quantized tensor is missing a part (a truncated or foreign file).
    pub fn check_complete(&self) -> Result<()> {
        match self.pending.keys().next() {
            Some(name) => Err(SdError::Weights(format!("quantized tensor `{name}` is incomplete"))),
            None => Ok(()),
        }
    }

    fn ingest(&mut self, name: String, dtype: &str, shape: Vec<usize>, data: &[u8]) -> Result<()> {
        let quant = [(".q4", 4u8), (".q8", 8), (".qs", 0), (".qshape", 1)].into_iter().find_map(|(suf, k)| name.strip_suffix(suf).map(|b| (b.to_string(), k)));
        let Some((base, kind)) = quant else {
            if matches!(dtype, "F16" | "BF16" | "F32") {
                return self.upload(name, dtype, shape, data);
            }
            return Ok(());
        };
        let part = self.pending.entry(base.clone()).or_default();
        match (kind, dtype) {
            (4, "U8") | (8, "I8") => part.values = Some((kind, data.to_vec())),
            (0, "F16") => part.scales = Some(data.as_chunks::<2>().0.iter().map(|b| half::f16::from_le_bytes(*b).to_f32()).collect()),
            (1, "I32") => part.shape = Some(data.as_chunks::<4>().0.iter().map(|b| usize::try_from(i32::from_le_bytes(*b)).unwrap_or(0)).collect()),
            _ => return Err(SdError::Weights(format!("`{name}`: unexpected dtype {dtype}"))),
        }
        if part.values.is_some() && part.scales.is_some() && part.shape.is_some()
            && let Some(p) = self.pending.remove(&base)
        {
            self.upload_quantized(base, p)?;
        }
        Ok(())
    }

    fn upload_quantized(&mut self, name: String, p: QuantParts) -> Result<()> {
        use burn::tensor::quantization::{QuantScheme, QuantStore, QuantValue};
        let (Some((bits, raw)), Some(scales), Some(shape)) = (p.values, p.scales, p.shape) else {
            return Err(SdError::Weights(format!("quantized tensor `{name}` is incomplete")));
        };
        let n = shape.iter().try_fold(1usize, |a, &d| a.checked_mul(d)).filter(|&n| n > 0 && n <= MAX_TENSOR);
        let rows = shape.first().copied().unwrap_or(0);
        let Some(n) = n.filter(|&n| shape.len() >= 2 && rows > 0 && (n / rows).is_multiple_of(QUANT_BLOCK)) else {
            return Err(SdError::Weights(format!("`{name}` has unsupported shape {shape:?}")));
        };
        let fan_in = n / rows;
        let values: Vec<i8> = if bits == 4 {
            if raw.len() * 2 != n {
                return Err(SdError::Weights(format!("`{name}`: {} packed bytes for {n} values", raw.len())));
            }
            raw.iter().flat_map(|b| [((b & 0x0F) << 4) as i8 >> 4, (b & 0xF0) as i8 >> 4]).collect()
        } else {
            if raw.len() != n {
                return Err(SdError::Weights(format!("`{name}`: {} bytes for {n} values", raw.len())));
            }
            raw.iter().map(|&b| b as i8).collect()
        };
        if scales.len() * QUANT_BLOCK != n || scales.iter().any(|s| !s.is_finite()) {
            return Err(SdError::Weights(format!("`{name}`: {} scales for {n} values", scales.len())));
        }
        // GPUs get dense weights in the compute precision (dequantized here: burn's packed
        // WebGPU dequantization fails on some drivers, and a GPU has the memory); the CPU keeps
        // one int8 per value and dequantizes each layer when it runs.
        if !is_cpu(&self.device) {
            let dense: Vec<f32> = values.iter().enumerate().map(|(i, &v)| f32::from(v) * scales.get(i / QUANT_BLOCK).copied().unwrap_or(0.0)).collect();
            let td = TensorData::new(dense, [n]);
            let td = if self.dtype == DType::F16 { td.convert_dtype(DType::F16) } else { td };
            let t = Tensor::<1>::from_data(td, TensorCreationOptions::new(self.device.clone()).with_dtype(self.dtype));
            self.bytes = self.bytes.saturating_add((n * if self.dtype == DType::F16 { 2 } else { 4 }) as u64);
            self.map.insert(name, (shape, t));
            return Ok(());
        }
        let store = QuantStore::Native;
        let value = if bits == 4 { QuantValue::Q4S } else { QuantValue::Q8S };
        let scheme = QuantScheme::default().with_value(value).with_store(store).per_block([QUANT_BLOCK as u8], burn::tensor::quantization::ScaleDtype::F32);
        let td = TensorData::quantized(values, [rows, fan_in], scheme, &scales, None);
        let t = Tensor::<2>::from_data(td, &self.device);
        self.bytes = self.bytes.saturating_add((n + scales.len() * 4) as u64);
        self.quant.insert(name, (shape, t));
        Ok(())
    }

    /// Uploads one tensor streamed by [`crate::stream::StreamParser`] (non-float tensors are
    /// skipped).
    pub fn add_raw(&mut self, t: &crate::stream::RawTensor) -> Result<()> {
        self.ingest(t.name.clone(), &t.dtype, t.shape.clone(), &t.data)
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
        self.quant.extend(other.quant);
    }

    /// Tensor names (unordered).
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }

    /// Whether a tensor is present.
    pub fn has(&self, name: &str) -> bool {
        self.map.contains_key(name) || self.quant.contains_key(name)
    }

    /// The shape of a tensor, if present.
    pub fn shape(&self, name: &str) -> Option<&[usize]> {
        self.map.get(name).map(|(s, _)| s.as_slice()).or_else(|| self.quant.get(name).map(|(s, _)| s.as_slice()))
    }

    /// A weight, kept quantized when it is.
    pub fn weight<const D: usize>(&self, name: &str, shape: [usize; D]) -> Result<Weight<D>> {
        match self.quant.get(name) {
            Some((s, q)) if s.as_slice() == shape.as_slice() => Ok(Weight::Quant(q.clone(), shape, self.dtype)),
            Some((s, _)) => Err(SdError::Weights(format!("`{name}` has shape {s:?}, expected {shape:?}"))),
            None => Ok(Weight::Dense(self.get(name, shape)?)),
        }
    }

    /// A tensor with exactly this shape.
    pub fn get<const D: usize>(&self, name: &str, shape: [usize; D]) -> Result<Tensor<D>> {
        if let Some((s, q)) = self.quant.get(name) {
            if s.as_slice() != shape.as_slice() {
                return Err(SdError::Weights(format!("`{name}` has shape {s:?}, expected {shape:?}")));
            }
            return Ok(q.clone().dequantize().cast(self.dtype).reshape(shape));
        }
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
        let w = self.weight(&format!("{name}.weight"), [d_out, d_in])?;
        let b = if bias { Some(self.get(&format!("{name}.bias"), [d_out])?) } else { None };
        Ok(Linear { w, b })
    }

    pub fn conv(&self, name: &str, c_in: usize, c_out: usize, k: usize) -> Result<Conv> {
        let w = self.weight(&format!("{name}.weight"), [c_out, c_in, k, k])?;
        let b = Some(self.get(&format!("{name}.bias"), [c_out])?);
        Ok(Conv { w, b, pad: k / 2 })
    }

    /// A convolution without bias.
    pub fn conv_nobias(&self, name: &str, c_in: usize, c_out: usize, k: usize) -> Result<Conv> {
        let w = self.weight(&format!("{name}.weight"), [c_out, c_in, k, k])?;
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

/// Whether `device` is the CPU backend.
fn is_cpu(device: &Device) -> bool {
    format!("{device:?}").to_ascii_lowercase().contains("flex")
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

/// A layer's weight: dense, or block-quantized `[out, fan_in]` dequantized at each use (the
/// light tiers keep 4/8-bit weights in memory; the transient f32 copy is one layer).
#[derive(Debug, Clone)]
pub enum Weight<const D: usize> {
    Dense(Tensor<D>),
    Quant(Tensor<2>, [usize; D], DType),
}

impl<const D: usize> Weight<D> {
    /// The dense weight (PyTorch layout).
    pub fn get(&self) -> Tensor<D> {
        match self {
            Self::Dense(t) => t.clone(),
            Self::Quant(q, shape, dt) => q.clone().dequantize().cast(*dt).reshape(*shape),
        }
    }

    pub fn dims(&self) -> [usize; D] {
        match self {
            Self::Dense(t) => t.dims(),
            Self::Quant(_, shape, _) => *shape,
        }
    }
}

/// `x·Wᵀ + b` over the last dimension (`W` stored PyTorch-style `[out, in]`).
#[derive(Debug, Clone)]
pub struct Linear {
    pub w: Weight<2>,
    pub b: Option<Tensor<1>>,
}

impl Linear {
    pub fn forward<const D: usize>(&self, x: Tensor<D>) -> Tensor<D> {
        burn::tensor::module::linear(x, self.w.get().transpose(), self.b.clone())
    }
}

/// A square convolution, stride 1, "same" zero padding.
#[derive(Debug, Clone)]
pub struct Conv {
    pub w: Weight<4>,
    pub b: Option<Tensor<1>>,
    pub pad: usize,
}

impl Conv {
    pub fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        conv2d(x, self.w.get(), self.b.clone(), ConvOptions::new([1, 1], [self.pad, self.pad], [1, 1], 1))
    }

    /// Stride 2 with the given padding (UNet: 1 on every side).
    pub fn forward_stride2(&self, x: Tensor<4>, pad: usize) -> Tensor<4> {
        conv2d(x, self.w.get(), self.b.clone(), ConvOptions::new([2, 2], [pad, pad], [1, 1], 1))
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
