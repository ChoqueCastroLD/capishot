//! Manga LaMa as an ONNX model, written from its weights in memory (for the DirectML backend,
//! `crate::directml`): the same graph as [`crate::Lama`]'s generator in fp32, for any working
//! size. Nothing is stored or downloaded: the bytes are built when the model loads.
//!
//! The Fourier unit is the same DFT matrix products as on burn ([`crate::dft::Dft2`]), as
//! `MatMul`s on the height and width axes. The matrices are graph inputs, so one session runs
//! every tile size; [`dft_inputs`] cuts them out of [`Dft2`] (the values burn uses):
//!
//! * `dc`, `ds` (`H × H`): `C_H/√H`, `S_H/√H`.
//! * `fwc`, `fws` (`W × Wc`): `C_W/√W`, `−S_W/√W`, so a row transform is `A = x·fwc`, `B = x·fws`.
//! * `iwc`, `iws` (`Wc × W`): the half-spectrum inverse's real and imaginary parts.
//!
//! The encoder writes only the protobuf messages an inference graph needs (`ModelProto`,
//! `GraphProto`, `NodeProto`, `AttributeProto`, `TensorProto`, `ValueInfoProto`), by field
//! number as `onnx.proto` defines them.

use crate::dft::Dft2;
use crate::weights::{Ffc, HostConv, Weights};

/// ONNX IR version 8 and operator set 17 (ONNX Runtime 1.14 and later).
const IR_VERSION: u64 = 8;
const OPSET: u64 = 17;
/// `TensorProto.DataType`: FLOAT, INT64.
const FLOAT: u64 = 1;
const INT64: u64 = 7;
/// The graph's image input and output.
pub const INPUT: &str = "input";
pub const OUTPUT: &str = "output";
/// The DFT matrix inputs, in [`dft_inputs`]' order, with their symbolic dimensions.
pub const DFT: [(&str, [&str; 2]); 6] =
    [("dc", ["hb", "hb"]), ("ds", ["hb", "hb"]), ("fwc", ["wb", "wc"]), ("fws", ["wb", "wc"]), ("iwc", ["wc", "wb"]), ("iws", ["wc", "wb"])];

/// A protobuf message under construction.
#[derive(Default)]
struct Msg(Vec<u8>);

impl Msg {
    fn varint(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }
    fn key(&mut self, field: u64, wire: u64) {
        self.varint((field << 3) | wire);
    }
    fn int(&mut self, field: u64, v: u64) -> &mut Self {
        self.key(field, 0);
        self.varint(v);
        self
    }
    fn bytes(&mut self, field: u64, b: &[u8]) -> &mut Self {
        self.key(field, 2);
        self.varint(b.len() as u64);
        self.0.extend_from_slice(b);
        self
    }
    fn str(&mut self, field: u64, s: &str) -> &mut Self {
        self.bytes(field, s.as_bytes())
    }
    fn msg(&mut self, field: u64, m: &Msg) -> &mut Self {
        self.bytes(field, &m.0)
    }
}

/// An `AttributeProto`.
enum Attr<'a> {
    Int(&'a str, i64),
    Ints(&'a str, &'a [i64]),
    Str(&'a str, &'a str),
}

impl Attr<'_> {
    fn encode(&self) -> Msg {
        let mut m = Msg::default();
        match self {
            // AttributeType: INT 2, STRING 3, INTS 7 (field 20).
            Self::Int(n, v) => m.str(1, n).int(3, *v as u64).int(20, 2),
            Self::Str(n, v) => m.str(1, n).bytes(4, v.as_bytes()).int(20, 3),
            Self::Ints(n, v) => {
                m.str(1, n);
                for x in v.iter() {
                    m.int(8, *x as u64);
                }
                m.int(20, 7)
            }
        };
        m
    }
}

/// A graph being built: nodes, initializers, fresh value names.
#[derive(Default)]
struct Graph {
    nodes: Vec<Msg>,
    inits: Vec<Msg>,
    n: usize,
}

impl Graph {
    fn fresh(&mut self, p: &str) -> String {
        self.n += 1;
        format!("{p}{}", self.n)
    }
    fn node_to(&mut self, op: &str, inputs: &[&str], attrs: &[Attr<'_>], outputs: &[&str]) {
        let mut m = Msg::default();
        for i in inputs {
            m.str(1, i);
        }
        for o in outputs {
            m.str(2, o);
        }
        m.str(3, outputs.first().copied().unwrap_or(op)).str(4, op);
        for a in attrs {
            m.msg(5, &a.encode());
        }
        self.nodes.push(m);
    }
    fn node(&mut self, op: &str, inputs: &[&str], attrs: &[Attr<'_>]) -> String {
        let out = self.fresh(&op.to_ascii_lowercase());
        self.node_to(op, inputs, attrs, &[&out]);
        out
    }
    fn tensor(&mut self, p: &str, dims: &[usize], ty: u64, raw: Vec<u8>) -> String {
        let name = self.fresh(p);
        let mut m = Msg::default();
        for d in dims {
            m.int(1, *d as u64);
        }
        m.int(2, ty).str(8, &name).bytes(9, &raw);
        self.inits.push(m);
        name
    }
    fn floats(&mut self, p: &str, dims: &[usize], data: &[f32]) -> String {
        self.tensor(p, dims, FLOAT, data.iter().flat_map(|v| v.to_le_bytes()).collect())
    }
    fn ints(&mut self, p: &str, data: &[i64]) -> String {
        self.tensor(p, &[data.len()], INT64, data.iter().flat_map(|v| v.to_le_bytes()).collect())
    }
    /// Reflect padding of `pad` pixels on each side of the spatial axes.
    fn reflect(&mut self, x: &str, pad: i64) -> String {
        let pads = self.ints("pads", &[0, 0, pad, pad, 0, 0, pad, pad]);
        self.node("Pad", &[x, &pads], &[Attr::Str("mode", "reflect")])
    }
    /// A convolution after reflect padding of `pad` (as `model.rs`'s `conv`).
    fn conv(&mut self, x: &str, c: &HostConv, stride: i64, pad: i64) -> String {
        let x = if pad > 0 { self.reflect(x, pad) } else { x.to_string() };
        let w = self.floats("w", &c.shape, &c.w);
        let mut inputs = vec![x, w];
        if let Some(b) = &c.b {
            inputs.push(self.floats("b", &[b.len()], b));
        }
        let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let k = [c.shape[2] as i64, c.shape[3] as i64];
        self.node("Conv", &refs, &[Attr::Ints("kernel_shape", &k), Attr::Ints("strides", &[stride, stride])])
    }
    fn relu(&mut self, x: &str) -> String {
        self.node("Relu", &[x], &[])
    }
    fn add(&mut self, a: &str, b: &str) -> String {
        self.node("Add", &[a, b], &[])
    }
    fn sub(&mut self, a: &str, b: &str) -> String {
        self.node("Sub", &[a, b], &[])
    }
    fn matmul(&mut self, a: &str, b: &str) -> String {
        self.node("MatMul", &[a, b], &[])
    }

    /// Fourier unit (see the module docs): rfft2 → 1×1 conv + ReLU on `[re, im]` → irfft2.
    fn fourier(&mut self, z: &str, fu: &HostConv) -> String {
        let [dc, ds, fwc, fws, iwc, iws] = DFT.map(|(n, _)| n);
        let (a, b) = (self.matmul(z, fwc), self.matmul(z, fws));
        let (ca, sb, cb, sa) = (self.matmul(dc, &a), self.matmul(ds, &b), self.matmul(dc, &b), self.matmul(ds, &a));
        let re = self.add(&ca, &sb);
        let im = self.sub(&cb, &sa);
        let spec = self.node("Concat", &[&re, &im], &[Attr::Int("axis", 1)]);
        let y = self.conv(&spec, fu, 1, 0);
        let y = self.relu(&y);
        let (r, i) = (self.fresh("re"), self.fresh("im"));
        // No `split` input: two equal halves (the channels are `[re…, im…]`).
        self.node_to("Split", &[&y], &[Attr::Int("axis", 1)], &[&r, &i]);
        let (cr, si, ci, sr) = (self.matmul(dc, &r), self.matmul(ds, &i), self.matmul(dc, &i), self.matmul(ds, &r));
        let gr = self.sub(&cr, &si);
        let gi = self.add(&ci, &sr);
        let (xr, xi) = (self.matmul(&gr, iwc), self.matmul(&gi, iws));
        self.add(&xr, &xi)
    }

    fn ffc(&mut self, xl: &str, xg: &str, f: &Ffc<HostConv>) -> (String, String) {
        let (l2l, g2l) = (self.conv(xl, &f.l2l, 1, 1), self.conv(xg, &f.g2l, 1, 1));
        let ol = self.add(&l2l, &g2l);
        let z = self.conv(xg, &f.st1, 1, 0);
        let z = self.relu(&z);
        let fz = self.fourier(&z, &f.fu);
        let s = self.add(&z, &fz);
        let st = self.conv(&s, &f.st2, 1, 0);
        let l2g = self.conv(xl, &f.l2g, 1, 1);
        let og = self.add(&l2g, &st);
        (self.relu(&ol), self.relu(&og))
    }
}

/// A `ValueInfoProto` for an fp32 tensor; `None` dimensions are free (named `sym`).
fn value(name: &str, dims: &[(Option<usize>, &str)]) -> Msg {
    let mut shape = Msg::default();
    for (fixed, sym) in dims {
        let mut dim = Msg::default();
        match fixed {
            Some(v) => dim.int(1, *v as u64),
            None => dim.str(2, sym),
        };
        shape.msg(1, &dim);
    }
    let mut tensor = Msg::default();
    tensor.int(1, FLOAT).msg(2, &shape);
    let mut ty = Msg::default();
    ty.msg(1, &tensor);
    let mut v = Msg::default();
    v.str(1, name).msg(2, &ty);
    v
}

/// The generator as ONNX model bytes: [`INPUT`] `[1, 4, H, W]` (see `model.rs`'s
/// `planar_input`) and the [`DFT`] matrices for `H/8 × W/8` → [`OUTPUT`] `[1, 3, H, W]` in 0..=1.
pub(crate) fn lama(w: &Weights<HostConv>) -> Vec<u8> {
    let mut g = Graph::default();
    let x = g.conv(INPUT, &w.stem, 1, 3);
    let x = g.relu(&x);
    let x = g.conv(&x, &w.down1, 2, 1);
    let x = g.relu(&x);
    let x = g.conv(&x, &w.down2, 2, 1);
    let x = g.relu(&x);
    let xl = g.conv(&x, &w.down3_l, 2, 1);
    let mut xl = g.relu(&xl);
    let xg = g.conv(&x, &w.down3_g, 2, 1);
    let mut xg = g.relu(&xg);
    for [c1, c2] in &w.blocks {
        let (al, ag) = g.ffc(&xl, &xg, c1);
        let (al, ag) = g.ffc(&al, &ag, c2);
        xl = g.add(&xl, &al);
        xg = g.add(&xg, &ag);
    }
    let mut x = g.node("Concat", &[&xl, &xg], &[Attr::Int("axis", 1)]);
    for u in &w.up {
        let wt = g.floats("w", &u.shape, &u.w);
        let mut inputs = vec![x.clone(), wt];
        if let Some(b) = &u.b {
            inputs.push(g.floats("b", &[b.len()], b));
        }
        let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let k = [u.shape[2] as i64, u.shape[3] as i64];
        let y = g.node(
            "ConvTranspose",
            &refs,
            &[Attr::Ints("kernel_shape", &k), Attr::Ints("strides", &[2, 2]), Attr::Ints("pads", &[1, 1, 1, 1]), Attr::Ints("output_padding", &[1, 1])],
        );
        x = g.relu(&y);
    }
    let y = g.conv(&x, &w.out, 1, 3);
    g.node_to("Sigmoid", &[&y], &[], &[OUTPUT]);

    let mut graph = Msg::default();
    for n in &g.nodes {
        graph.msg(1, n);
    }
    graph.str(2, "manga-lama");
    for i in &g.inits {
        graph.msg(5, i);
    }
    graph.msg(11, &value(INPUT, &[(Some(1), ""), (Some(4), ""), (None, "h"), (None, "w")]));
    for (name, [r, c]) in DFT {
        graph.msg(11, &value(name, &[(None, r), (None, c)]));
    }
    graph.msg(12, &value(OUTPUT, &[(Some(1), ""), (Some(3), ""), (None, "h"), (None, "w")]));
    let mut opset = Msg::default();
    opset.str(1, "").int(2, OPSET);
    let mut model = Msg::default();
    model.int(1, IR_VERSION).str(2, "capishot").msg(7, &graph).msg(8, &opset);
    model.0
}

/// The [`DFT`] inputs for a `bh × bw` bottleneck: `(name, [rows, cols], values)`, cut out of
/// [`Dft2`]'s matrices (so the values burn uses).
pub(crate) fn dft_inputs(d: &Dft2) -> [(&'static str, [usize; 2], Vec<f32>); 6] {
    let (h, w, wc) = (d.h, d.w, d.wc());
    let n2 = 2 * h;
    let at = |m: &[f32], i: usize| m.get(i).copied().unwrap_or(0.0);
    let grid = |rows: usize, cols: usize, f: &dyn Fn(usize, usize) -> f32| -> Vec<f32> {
        (0..rows).flat_map(|r| (0..cols).map(move |c| (r, c))).map(|(r, c)| f(r, c)).collect()
    };
    // fh = [[C, −S], [S, C]] / √H; fw = [C | −S] / √W per row; iw = [real rows; imaginary rows].
    let dc = grid(h, h, &|i, j| at(&d.fh, i * n2 + j));
    let ds = grid(h, h, &|i, j| at(&d.fh, (h + i) * n2 + j));
    let fwc = grid(w, wc, &|n, k| at(&d.fw, n * 2 * wc + k));
    let fws = grid(w, wc, &|n, k| at(&d.fw, n * 2 * wc + wc + k));
    let iwc = grid(wc, w, &|k, n| at(&d.iw, k * w + n));
    let iws = grid(wc, w, &|k, n| at(&d.iw, (wc + k) * w + n));
    let [a, b, c, e, f, g] = DFT.map(|(n, _)| n);
    [(a, [h, h], dc), (b, [h, h], ds), (c, [w, wc], fwc), (e, [w, wc], fws), (f, [wc, w], iwc), (g, [wc, w], iws)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matmul(a: &[f32], n: usize, k: usize, b: &[f32], m: usize) -> Vec<f32> {
        let mut out = vec![0f32; n * m];
        for i in 0..n {
            for j in 0..m {
                out[i * m + j] = (0..k).map(|t| a[i * k + t] * b[t * m + j]).sum();
            }
        }
        out
    }

    /// The graph's Fourier products, run by hand on the inputs [`dft_inputs`] gives, match
    /// [`Dft2`]'s own forward and inverse transforms (what burn computes), for odd and even sizes.
    #[test]
    fn dft_inputs_reproduce_the_burn_transform() {
        for (h, w) in [(6, 8), (5, 7), (8, 13)] {
            let d = Dft2::new(h, w).unwrap();
            let wc = d.wc();
            let [(_, _, dc), (_, _, ds), (_, _, fwc), (_, _, fws), (_, _, iwc), (_, _, iws)] = dft_inputs(&d);
            let x: Vec<f32> = (0..h * w).map(|i| ((i * 37 % 11) as f32 - 5.0) / 3.0).collect();
            let (a, b) = (matmul(&x, h, w, &fwc, wc), matmul(&x, h, w, &fws, wc));
            let re: Vec<f32> = matmul(&dc, h, h, &a, wc).iter().zip(matmul(&ds, h, h, &b, wc)).map(|(p, q)| p + q).collect();
            let im: Vec<f32> = matmul(&dc, h, h, &b, wc).iter().zip(matmul(&ds, h, h, &a, wc)).map(|(p, q)| p - q).collect();
            // Dft2's layout: wc rows of [re over y | im over y].
            let want = d.forward_ref(&x).unwrap();
            for k in 0..wc {
                for y in 0..h {
                    assert!((want[k * 2 * h + y] - re[y * wc + k]).abs() < 1e-4, "re {h}x{w}");
                    assert!((want[k * 2 * h + h + y] - im[y * wc + k]).abs() < 1e-4, "im {h}x{w}");
                }
            }
            let gr: Vec<f32> = matmul(&dc, h, h, &re, wc).iter().zip(matmul(&ds, h, h, &im, wc)).map(|(p, q)| p - q).collect();
            let gi: Vec<f32> = matmul(&dc, h, h, &im, wc).iter().zip(matmul(&ds, h, h, &re, wc)).map(|(p, q)| p + q).collect();
            let back: Vec<f32> = matmul(&gr, h, wc, &iwc, w).iter().zip(matmul(&gi, h, wc, &iws, w)).map(|(p, q)| p + q).collect();
            let want = d.inverse_ref(&want).unwrap();
            for (i, (p, q)) in back.iter().zip(&want).enumerate() {
                assert!((p - q).abs() < 1e-4, "inverse {h}x{w} at {i}: {p} vs {q}");
            }
            for (p, q) in back.iter().zip(&x) {
                assert!((p - q).abs() < 1e-3, "round trip {h}x{w}");
            }
        }
    }

    #[test]
    fn varints() {
        let mut m = Msg::default();
        m.varint(300);
        assert_eq!(m.0, [0xac, 0x02]);
    }
}
