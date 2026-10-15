//! Loading the converted (batch-norm-folded) generator weights from safetensors.
//!
//! Every tensor's name, dtype and exact shape is checked against the architecture before any
//! device allocation, so a truncated or foreign file is an error, never a panic in burn.

use burn::tensor::{Device, Tensor, TensorData};
use safetensors::{Dtype, SafeTensors};

use crate::{LamaError, Result};

/// The generator's size: `ngf` base channels (64 for big-lama) and the number of FFC ResNet
/// blocks (18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arch {
    /// Channels after the stem (`ngf`); the bottleneck has `8·ngf`.
    pub ngf: usize,
    /// FFC ResNet blocks.
    pub blocks: usize,
}

impl Arch {
    /// Bottleneck channels, local part (`ratio 0.25`).
    pub fn cl(&self) -> usize {
        self.ngf * 8 / 4
    }
    /// Bottleneck channels, global (spectral) part.
    pub fn cg(&self) -> usize {
        self.ngf * 8 - self.cl()
    }
}

/// A convolution with its (folded) bias.
#[derive(Debug, Clone)]
pub(crate) struct Conv {
    pub w: Tensor<4>,
    pub b: Option<Tensor<1>>,
}

/// One FFC + folded BN + ReLU, global ratio 0.75 in and out.
#[derive(Debug, Clone)]
pub(crate) struct Ffc {
    pub l2l: Conv,
    pub g2l: Conv,
    pub l2g: Conv,
    /// Spectral transform: 1×1 reduce (+BN+ReLU).
    pub st1: Conv,
    /// Fourier unit 1×1 conv on `[re…, im…]` (+BN+ReLU).
    pub fu: Conv,
    /// Spectral transform: 1×1 expand (scaled by the FFC's global BN).
    pub st2: Conv,
}

#[derive(Debug, Clone)]
pub(crate) struct Weights {
    pub arch: Arch,
    pub stem: Conv,
    pub down1: Conv,
    pub down2: Conv,
    pub down3_l: Conv,
    pub down3_g: Conv,
    pub blocks: Vec<[Ffc; 2]>,
    pub up: [Conv; 3],
    pub out: Conv,
}

/// Upper bounds that keep a hostile header from asking for absurd allocations.
const MAX_NGF: usize = 256;
const MAX_BLOCKS: usize = 64;

struct Reader<'a> {
    st: SafeTensors<'a>,
    device: Device,
}

impl Reader<'_> {
    fn floats(&self, name: &str, shape: &[usize]) -> Result<Vec<f32>> {
        let t = self.st.tensor(name).map_err(|_| LamaError::Weights(format!("missing tensor `{name}`")))?;
        if t.shape() != shape {
            return Err(LamaError::Weights(format!("`{name}` has shape {:?}, expected {shape:?}", t.shape())));
        }
        let n: usize = shape.iter().product();
        let data = t.data();
        let v: Vec<f32> = match t.dtype() {
            Dtype::F32 if data.len() == n * 4 => data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
            Dtype::F16 if data.len() == n * 2 => data.as_chunks::<2>().0.iter().map(|b| half::f16::from_le_bytes(*b).to_f32()).collect(),
            Dtype::BF16 if data.len() == n * 2 => data.as_chunks::<2>().0.iter().map(|b| half::bf16::from_le_bytes(*b).to_f32()).collect(),
            other => {
                return Err(LamaError::Weights(format!("`{name}`: unsupported dtype {other:?} or wrong byte length")));
            }
        };
        if v.iter().any(|x| !x.is_finite()) {
            return Err(LamaError::Weights(format!("`{name}` has non-finite values")));
        }
        Ok(v)
    }

    fn t4(&self, name: &str, shape: [usize; 4]) -> Result<Tensor<4>> {
        let v = self.floats(name, &shape)?;
        Ok(Tensor::from_data(TensorData::new(v, shape), &self.device))
    }

    fn t1(&self, name: &str, len: usize) -> Result<Tensor<1>> {
        let v = self.floats(name, &[len])?;
        Ok(Tensor::from_data(TensorData::new(v, [len]), &self.device))
    }

    /// `name.w` with `shape` and, when `bias`, `name.b` with `shape[bias_dim]` entries.
    fn conv(&self, name: &str, shape: [usize; 4], bias: Option<usize>) -> Result<Conv> {
        let w = self.t4(&format!("{name}.w"), shape)?;
        let b = match bias {
            Some(n) => Some(self.t1(&format!("{name}.b"), n)?),
            None => None,
        };
        Ok(Conv { w, b })
    }
}

/// Parses and uploads the weights.
pub(crate) fn load(bytes: &[u8], device: &Device) -> Result<Weights> {
    let (_, header) = SafeTensors::read_metadata(bytes).map_err(|e| LamaError::Weights(format!("not a safetensors file: {e}")))?;
    let meta = header.metadata().as_ref();
    if let Some(arch) = meta.and_then(|m| m.get("arch"))
        && arch != "lama-ffc"
    {
        return Err(LamaError::Weights(format!("architecture `{arch}`, expected `lama-ffc`")));
    }
    if let Some(layout) = meta.and_then(|m| m.get("layout"))
        && layout != "bn-folded-v1"
    {
        return Err(LamaError::Weights(format!("layout `{layout}`, expected `bn-folded-v1`")));
    }
    let st = SafeTensors::deserialize(bytes).map_err(|e| LamaError::Weights(format!("not a safetensors file: {e}")))?;
    // ngf from the stem's shape, blocks from the names (metadata only cross-checks).
    let ngf = st.tensor("in.w").ok().and_then(|t| t.shape().first().copied()).ok_or_else(|| LamaError::Weights("missing tensor `in.w`".into()))?;
    if ngf == 0 || ngf > MAX_NGF || !ngf.is_multiple_of(4) {
        return Err(LamaError::Weights(format!("unsupported ngf {ngf}")));
    }
    let mut blocks = 0;
    while blocks <= MAX_BLOCKS && st.tensor(&format!("blocks.{blocks}.conv1.l2l.w")).is_ok() {
        blocks += 1;
    }
    if blocks == 0 || blocks > MAX_BLOCKS {
        return Err(LamaError::Weights(format!("unsupported block count {blocks}")));
    }
    for (key, value) in [("ngf", ngf), ("blocks", blocks)] {
        if let Some(m) = meta.and_then(|m| m.get(key)).and_then(|v| v.parse::<usize>().ok())
            && m != value
        {
            return Err(LamaError::Weights(format!("metadata {key}={m} but tensors say {value}")));
        }
    }
    let arch = Arch { ngf, blocks };
    let r = Reader { st, device: device.clone() };
    let (c, cl, cg) = (ngf * 8, arch.cl(), arch.cg());
    let ch = cg / 2;
    let stem = r.conv("in", [ngf, 4, 7, 7], Some(ngf))?;
    let down1 = r.conv("down1", [ngf * 2, ngf, 3, 3], Some(ngf * 2))?;
    let down2 = r.conv("down2", [ngf * 4, ngf * 2, 3, 3], Some(ngf * 4))?;
    let down3_l = r.conv("down3.l", [cl, ngf * 4, 3, 3], Some(cl))?;
    let down3_g = r.conv("down3.g", [cg, ngf * 4, 3, 3], Some(cg))?;
    let ffc = |p: &str| -> Result<Ffc> {
        Ok(Ffc {
            l2l: r.conv(&format!("{p}.l2l"), [cl, cl, 3, 3], Some(cl))?,
            g2l: r.conv(&format!("{p}.g2l"), [cl, cg, 3, 3], None)?,
            l2g: r.conv(&format!("{p}.l2g"), [cg, cl, 3, 3], Some(cg))?,
            st1: r.conv(&format!("{p}.st1"), [ch, cg, 1, 1], Some(ch))?,
            fu: r.conv(&format!("{p}.fu"), [2 * ch, 2 * ch, 1, 1], Some(2 * ch))?,
            st2: r.conv(&format!("{p}.st2"), [cg, ch, 1, 1], None)?,
        })
    };
    let mut bl = Vec::with_capacity(blocks);
    for i in 0..blocks {
        bl.push([ffc(&format!("blocks.{i}.conv1"))?, ffc(&format!("blocks.{i}.conv2"))?]);
    }
    let up =
        [r.conv("up1", [c, c / 2, 3, 3], Some(c / 2))?, r.conv("up2", [c / 2, c / 4, 3, 3], Some(c / 4))?, r.conv("up3", [c / 4, c / 8, 3, 3], Some(c / 8))?];
    let out = r.conv("out", [3, ngf, 7, 7], Some(3))?;
    Ok(Weights { arch, stem, down1, down2, down3_l, down3_g, blocks: bl, up, out })
}
