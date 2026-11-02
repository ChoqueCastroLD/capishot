//! FDAT ([`crate::fdat`]) as an ONNX graph for one input size, in fp32 from the same weights as
//! the burn network, op for op: tokens `[1, L, C]` (L = H·W row-major) as burn keeps them,
//! convolutions through `[1, C, H, W]`. Every shape is a constant (reshapes, the window padding
//! and its crop), which is what a DirectML session per tile size wants anyway.
//!
//! Faithful to burn where it matters for the output:
//! * window attention zero-pads the normalised tokens bottom and right to multiples of the
//!   window and crops back afterwards; the padded tokens are not masked (they still take part
//!   in their window's softmax, as in burn and spandrel);
//! * the spatial scale `head_dim^-0.5` is folded into the q weights (burn scales q before the
//!   product), the learned bias added after it, before the softmax;
//! * channel attention L2-normalises q and k over the tokens (clamped at 1e-12), scales by the
//!   per-head temperature, and puts the result back as channel = head·hd + d;
//! * GELU is the exact one, x·½·(1 + erf(x/√2)); nothing is clamped (the caller clamps).

use super::{Attr, Dim, Graph, INPUT, OUTPUT, space_to_depth_order, value};
use crate::fdat::{Config, Fdat, HEADS, LN_EPS, NORM_EPS, WINDOW};
use crate::weights::Weights;
use crate::{AiError, Result};

/// Tile sides past this are refused (a graph is built per size; no tile gets near it).
const MAX_SIDE: usize = 4096;

/// The builder's state: the graph, the weights, the network's sizes and shared constants.
struct Fb<'a> {
    g: Graph,
    w: &'a Weights,
    /// Channels and the network's (unshuffled) height and width.
    c: usize,
    h: usize,
    wd: usize,
    half: String,
    one: String,
    isqrt2: String,
}

fn dims(v: &[usize]) -> Vec<i64> {
    v.iter().map(|&d| d as i64).collect()
}

/// `[out, in]` (a torch `nn.Linear` weight, or a 1×1 conv's) as `[in, out]` for `MatMul`.
fn transposed(data: &[f32], o: usize, i: usize) -> Vec<f32> {
    let mut t = vec![0.0f32; o * i];
    for r in 0..o {
        for col in 0..i {
            if let (Some(dst), Some(src)) = (t.get_mut(col * o + r), data.get(r * i + col)) {
                *dst = *src;
            }
        }
    }
    t
}

impl Fb<'_> {
    fn data(&self, name: &str, shape: &[usize]) -> Result<Vec<f32>> {
        Ok(self.w.get(name, shape)?.data.clone())
    }
    fn has(&self, name: &str) -> bool {
        self.w.tensors.contains_key(name)
    }
    fn op(&mut self, op: &str, inputs: &[&str], attrs: &[Attr<'_>]) -> String {
        self.g.node(op, inputs, attrs)
    }
    fn reshape(&mut self, x: &str, shape: &[usize]) -> String {
        let s = self.g.ints("shape", &dims(shape));
        self.op("Reshape", &[x, &s], &[])
    }
    fn transpose(&mut self, x: &str, perm: &[i64]) -> String {
        self.op("Transpose", &[x], &[Attr::Ints("perm", perm)])
    }
    fn gelu(&mut self, x: &str) -> String {
        let (half, one, isqrt2) = (self.half.clone(), self.one.clone(), self.isqrt2.clone());
        let s = self.op("Mul", &[x, &isqrt2], &[]);
        let e = self.op("Erf", &[&s], &[]);
        let e1 = self.op("Add", &[&e, &one], &[]);
        let xh = self.op("Mul", &[x, &half], &[]);
        self.op("Mul", &[&xh, &e1], &[])
    }
    /// `x @ wᵀ (+ b)` for a torch weight `[out, in]` given as data.
    fn matmul(&mut self, x: &str, w: &[f32], o: usize, i: usize, b: Option<&[f32]>) -> String {
        let wt = self.g.init("w", &[i, o], &transposed(w, o, i));
        let y = self.op("MatMul", &[x, &wt], &[]);
        match b {
            Some(b) => {
                let bn = self.g.init("b", &[o], b);
                self.op("Add", &[&y, &bn], &[])
            }
            None => y,
        }
    }
    /// `nn.Linear` `name` (`[out, in]`, bias if the weights have one).
    fn linear(&mut self, x: &str, name: &str, o: usize, i: usize) -> Result<String> {
        let w = self.data(&format!("{name}.weight"), &[o, i])?;
        let b = if self.has(&format!("{name}.bias")) { Some(self.data(&format!("{name}.bias"), &[o])?) } else { None };
        Ok(self.matmul(x, &w, o, i, b.as_deref()))
    }
    /// A 1×1 convolution `name` (`[o, i, 1, 1]`, no bias) applied to tokens as a matmul.
    fn pointwise(&mut self, x: &str, name: &str, o: usize, i: usize) -> Result<String> {
        let w = self.data(&format!("{name}.weight"), &[o, i, 1, 1])?;
        Ok(self.matmul(x, &w, o, i, None))
    }
    /// A `k×k` convolution `name` (padding `k/2`), `groups` groups, its bias when `bias`.
    fn conv(&mut self, x: &str, name: &str, shape: [usize; 4], groups: usize, bias: bool, out: Option<&str>) -> Result<String> {
        let k = shape[2];
        let w = self.data(&format!("{name}.weight"), &shape)?;
        let wn = self.g.init("w", &shape, &w);
        let mut inputs = vec![x.to_string(), wn];
        if bias {
            let b = self.data(&format!("{name}.bias"), &shape[..1])?;
            inputs.push(self.g.init("b", &shape[..1], &b));
        }
        let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let p = (k / 2) as i64;
        let kk = k as i64;
        let attrs = [Attr::Ints("kernel_shape", &[kk, kk]), Attr::Ints("pads", &[p, p, p, p]), Attr::Int("group", groups as i64)];
        let out = out.map_or_else(|| self.g.fresh("conv"), str::to_string);
        Ok(self.g.node_to("Conv", &refs, &attrs, out))
    }
    fn layer_norm(&mut self, x: &str, name: &str) -> Result<String> {
        let c = self.c;
        let w = self.data(&format!("{name}.weight"), &[c])?;
        let b = self.data(&format!("{name}.bias"), &[c])?;
        let (wn, bn) = (self.g.init("w", &[c], &w), self.g.init("b", &[c], &b));
        Ok(self.op("LayerNormalization", &[x, &wn, &bn], &[Attr::Int("axis", -1), Attr::Float("epsilon", LN_EPS as f32)]))
    }
    /// `[1, L, ch]` → `[1, ch, H, W]`.
    fn image(&mut self, x: &str, ch: usize) -> String {
        let t = self.transpose(x, &[0, 2, 1]);
        self.reshape(&t, &[1, ch, self.h, self.wd])
    }
    /// `[1, ch, H, W]` → `[1, L, ch]`.
    fn tokens(&mut self, x: &str, ch: usize) -> String {
        let r = self.reshape(x, &[1, ch, self.h * self.wd]);
        self.transpose(&r, &[0, 2, 1])
    }
    /// q, k, v of `qkv` (`[3C, C]` and its bias if any), each `[.., C]` from tokens `x`, the q
    /// weights scaled by `q_scale`.
    fn qkv(&mut self, x: &str, p: &str, q_scale: f32) -> Result<[String; 3]> {
        let c = self.c;
        let w = self.data(&format!("{p}.attn.qkv.weight"), &[3 * c, c])?;
        let b = if self.has(&format!("{p}.attn.qkv.bias")) { Some(self.data(&format!("{p}.attn.qkv.bias"), &[3 * c])?) } else { None };
        let mut out: [String; 3] = Default::default();
        for (i, slot) in out.iter_mut().enumerate() {
            let rows = w.get(i * c * c..(i + 1) * c * c).ok_or_else(|| AiError::Weights(format!("{p}.attn.qkv.weight is short")))?;
            let bias = b.as_ref().and_then(|b| b.get(i * c..(i + 1) * c));
            let s = if i == 0 { q_scale } else { 1.0 };
            let rows: Vec<f32> = rows.iter().map(|v| v * s).collect();
            let bias: Option<Vec<f32>> = bias.map(|b| b.iter().map(|v| v * s).collect());
            *slot = self.matmul(x, &rows, c, c, bias.as_deref());
        }
        Ok(out)
    }

    /// Window attention over tokens `x` (`[1, L, C]`, normalised) → `[1, L, C]`.
    fn spatial(&mut self, x: &str, p: &str) -> Result<String> {
        let (c, h, w, ws) = (self.c, self.h, self.wd, WINDOW);
        let hd = c / HEADS;
        let (pb, pr) = ((ws - h % ws) % ws, (ws - w % ws) % ws);
        let (hp, wp) = (h + pb, w + pr);
        let (nh, nw) = (hp / ws, wp / ws);
        let (n, nwin) = (ws * ws, nh * nw);
        let mut img = self.reshape(x, &[1, h, w, c]);
        if pb > 0 || pr > 0 {
            let pads = self.g.ints("pads", &[0, 0, 0, 0, 0, pb as i64, pr as i64, 0]);
            img = self.op("Pad", &[&img, &pads], &[Attr::Str("mode", "constant")]);
        }
        let t = self.reshape(&img, &[nh, ws, nw, ws, c]);
        let t = self.transpose(&t, &[0, 2, 1, 3, 4]);
        let win = self.reshape(&t, &[nwin, n, c]);
        let scale = (hd as f64).powf(-0.5) as f32;
        let [q, k, v] = self.qkv(&win, p, scale)?;
        let heads = |s: &mut Self, t: &str, perm: &[i64]| {
            let r = s.reshape(t, &[nwin, n, HEADS, hd]);
            s.transpose(&r, perm)
        };
        let q = heads(self, &q, &[0, 2, 1, 3]);
        let kt = heads(self, &k, &[0, 2, 3, 1]);
        let v = heads(self, &v, &[0, 2, 1, 3]);
        let bias = self.data(&format!("{p}.attn.bias"), &[HEADS, n, n])?;
        let bias = self.g.init("bias", &[1, HEADS, n, n], &bias);
        let a = self.op("MatMul", &[&q, &kt], &[]);
        let a = self.op("Add", &[&a, &bias], &[]);
        let a = self.op("Softmax", &[&a], &[Attr::Int("axis", -1)]);
        let o = self.op("MatMul", &[&a, &v], &[]);
        let o = self.transpose(&o, &[0, 2, 1, 3]);
        let o = self.reshape(&o, &[nwin, n, c]);
        let o = self.linear(&o, &format!("{p}.attn.proj"), c, c)?;
        let o = self.reshape(&o, &[nh, nw, ws, ws, c]);
        let o = self.transpose(&o, &[0, 2, 1, 3, 4]);
        let mut o = self.reshape(&o, &[1, hp, wp, c]);
        if pb > 0 || pr > 0 {
            let (starts, ends, axes) = (self.g.ints("starts", &[0, 0]), self.g.ints("ends", &[h as i64, w as i64]), self.g.ints("axes", &[1, 2]));
            o = self.op("Slice", &[&o, &starts, &ends, &axes], &[]);
        }
        Ok(self.reshape(&o, &[1, h * w, c]))
    }

    /// Channel attention over tokens `x` (`[1, L, C]`, normalised) → `[1, L, C]`.
    fn channel(&mut self, x: &str, p: &str) -> Result<String> {
        let c = self.c;
        let hd = c / HEADS;
        let l = self.h * self.wd;
        let [q, k, v] = self.qkv(x, p, 1.0)?;
        let eps = self.g.init("eps", &[], &[NORM_EPS as f32]);
        // [1, L, C] → [1, heads, hd, L]
        let heads = |s: &mut Self, t: &str| {
            let r = s.reshape(t, &[1, l, HEADS, hd]);
            s.transpose(&r, &[0, 2, 3, 1])
        };
        let l2n = |s: &mut Self, t: &str| {
            let n = s.op("ReduceL2", &[t], &[Attr::Ints("axes", &[-1]), Attr::Int("keepdims", 1)]);
            let n = s.op("Max", &[&n, &eps], &[]);
            s.op("Div", &[t, &n], &[])
        };
        let q = heads(self, &q);
        let q = l2n(self, &q);
        let k = heads(self, &k);
        let k = l2n(self, &k);
        let vt = heads(self, &v);
        let kt = self.transpose(&k, &[0, 1, 3, 2]);
        let a = self.op("MatMul", &[&q, &kt], &[]);
        let temp = self.data(&format!("{p}.attn.temp"), &[HEADS, 1, 1])?;
        let temp = self.g.init("temp", &[1, HEADS, 1, 1], &temp);
        let a = self.op("Mul", &[&a, &temp], &[]);
        let a = self.op("Softmax", &[&a], &[Attr::Int("axis", -1)]);
        let o = self.op("MatMul", &[&a, &vt], &[]);
        let o = self.transpose(&o, &[0, 3, 1, 2]);
        let o = self.reshape(&o, &[1, l, c]);
        self.linear(&o, &format!("{p}.attn.proj"), c, c)
    }

    fn block(&mut self, x: &str, p: &str, hidden: usize, red: usize) -> Result<String> {
        let c = self.c;
        let n1 = self.layer_norm(x, &format!("{p}.n1"))?;
        let is_channel = self.has(&format!("{p}.attn.temp"));
        let a = if is_channel { self.channel(&n1, p)? } else { self.spatial(&n1, p)? };
        let img = self.image(&n1, c);
        let cf = self.conv(&img, &format!("{p}.conv.0"), [c, 1, 3, 3], c, false, None)?;
        let cf = self.gelu(&cf);
        let cf = self.tokens(&cf, c);
        let fused = if is_channel {
            // A spatial gate from the attention scales the conv branch.
            let sm = self.pointwise(&a, &format!("{p}.inter.sg.0"), 1, c)?;
            let sm = self.op("Sigmoid", &[&sm], &[]);
            let m = self.op("Mul", &[&cf, &sm], &[]);
            self.op("Add", &[&a, &m], &[])
        } else {
            // A channel gate from the conv branch scales the attention.
            let pooled = self.op("ReduceMean", &[&cf], &[Attr::Ints("axes", &[1]), Attr::Int("keepdims", 1)]);
            let c1 = self.pointwise(&pooled, &format!("{p}.inter.cg.1"), red, c)?;
            let c1 = self.gelu(&c1);
            let cm = self.pointwise(&c1, &format!("{p}.inter.cg.3"), c, red)?;
            let cm = self.op("Sigmoid", &[&cm], &[]);
            let m = self.op("Mul", &[&a, &cm], &[]);
            self.op("Add", &[&m, &cf], &[])
        };
        let x = self.op("Add", &[x, &fused], &[]);
        let n2 = self.layer_norm(&x, &format!("{p}.n2"))?;
        let f = self.linear(&n2, &format!("{p}.ffn.fc1"), hidden, c)?;
        let f = self.gelu(&f);
        let fi = self.image(&f, hidden);
        let fi = self.conv(&fi, &format!("{p}.ffn.smix"), [hidden, 1, 3, 3], hidden, false, None)?;
        let f = self.tokens(&fi, hidden);
        let f = self.linear(&f, &format!("{p}.ffn.fc2"), c, hidden)?;
        Ok(self.op("Add", &[&x, &f], &[]))
    }

    /// A stride-2 4×4 transposed convolution `name` (padding 1, weight `[in, out, 4, 4]`).
    fn up(&mut self, x: &str, name: &str, i: usize, o: usize) -> Result<String> {
        let w = self.data(&format!("{name}.weight"), &[i, o, 4, 4])?;
        let b = self.data(&format!("{name}.bias"), &[o])?;
        let (wn, bn) = (self.g.init("w", &[i, o, 4, 4], &w), self.g.init("b", &[o], &b));
        let attrs = [Attr::Ints("kernel_shape", &[4, 4]), Attr::Ints("strides", &[2, 2]), Attr::Ints("pads", &[1, 1, 1, 1])];
        Ok(self.op("ConvTranspose", &[x, &wn, &bn], &attrs))
    }
}

/// FDAT as ONNX model bytes for inputs of exactly `size` (`w`, `h`; multiples of the unshuffle
/// factor): `input` `[1, 3, h, w]` → `output` `[1, out, h·scale, w·scale]`, fp32 throughout.
pub(crate) fn fdat(w: &Weights, size: (usize, usize)) -> Result<Vec<u8>> {
    let cfg: Config = Fdat::config(w)?;
    let (iw, ih) = size;
    let r = cfg.shuffle;
    if iw == 0 || ih == 0 || iw > MAX_SIDE || ih > MAX_SIDE || iw % r != 0 || ih % r != 0 {
        return Err(AiError::Image(format!("FDAT can't take a {iw}×{ih} input (×{} network)", cfg.scale())));
    }
    let c = cfg.dim;
    let mut g = Graph::default();
    let half = g.init("half", &[], &[0.5]);
    let one = g.init("one", &[], &[1.0]);
    let isqrt2 = g.init("isqrt2", &[], &[std::f32::consts::FRAC_1_SQRT_2]);
    let mut b = Fb { g, w, c, h: ih / r, wd: iw / r, half, one, isqrt2 };
    let hidden = w.shape("groups.0.blocks.0.ffn.fc1.weight").and_then(|s| s.first().copied()).unwrap_or(2 * c);
    let red = w.shape("groups.0.blocks.0.inter.cg.1.weight").and_then(|s| s.first().copied()).unwrap_or(c / 8);
    let out_c = w.shape("upsampler.2.weight").and_then(|s| s.get(1).copied()).unwrap_or(3);

    let shallow = if r > 1 {
        let shape = [c, 3 * r * r, 3, 3];
        let first = space_to_depth_order(&b.data("conv_first.1.weight", &shape)?, shape, 3, r);
        let x = b.op("SpaceToDepth", &[INPUT], &[Attr::Int("blocksize", r as i64)]);
        let wn = b.g.init("w", &shape, &first);
        let bias = b.data("conv_first.1.bias", &[c])?;
        let bn = b.g.init("b", &[c], &bias);
        b.op("Conv", &[&x, &wn, &bn], &[Attr::Ints("kernel_shape", &[3, 3]), Attr::Ints("pads", &[1, 1, 1, 1])])
    } else {
        b.conv(INPUT, "conv_first", [c, 3, 3, 3], 1, true, None)?
    };
    let mut deep = shallow.clone();
    for gi in 0..cfg.groups {
        let mut seq = b.tokens(&deep, c);
        for bi in 0..cfg.blocks {
            seq = b.block(&seq, &format!("groups.{gi}.blocks.{bi}"), hidden, red)?;
        }
        let img = b.image(&seq, c);
        let y = b.conv(&img, &format!("groups.{gi}.conv"), [c, c, 3, 3], 1, false, None)?;
        deep = b.op("Add", &[&y, &deep], &[]);
    }
    let y = b.conv(&deep, "conv_after", [c, c, 3, 3], 1, false, None)?;
    let x = b.op("Add", &[&y, &shallow], &[]);
    let x = b.up(&x, "upsampler.0", c, c)?;
    let x = b.gelu(&x);
    let x = b.up(&x, "upsampler.2", c, out_c)?;
    b.conv(&x, "upsampler.3", [out_c, out_c, 3, 3], 1, true, Some(OUTPUT))?;
    let s = cfg.scale();
    let input = value(INPUT, &[Dim::Fixed(1), Dim::Fixed(3), Dim::Fixed(ih), Dim::Fixed(iw)]);
    let output = value(OUTPUT, &[Dim::Fixed(1), Dim::Fixed(out_c), Dim::Fixed(ih * s), Dim::Fixed(iw * s)]);
    Ok(b.g.model("fdat", &input, &output))
}
