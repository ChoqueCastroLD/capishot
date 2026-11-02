//! The upscalers as ONNX models, written from their weights in memory (for the DirectML backend,
//! `crate::directml`): ESRGAN, the same graph as [`crate::esrgan::Esrgan::forward`] in fp32 with
//! the grayscale folding already in the weights, and FDAT ([`fdat`]), the same graph as
//! [`crate::fdat::Fdat`]'s forward, built per input size. Nothing is stored or downloaded: the
//! model bytes are built when the network loads (or a tile size first runs) and handed to ONNX
//! Runtime.
//!
//! The encoder writes only the few protobuf messages an inference graph needs (`ModelProto`,
//! `GraphProto`, `NodeProto`, `AttributeProto`, `TensorProto`, `ValueInfoProto`), by field
//! number as `onnx.proto` defines them.

use crate::compact::HostCompact;
use crate::esrgan::{HostConv, HostEsrgan};

mod fdat;
pub(crate) use fdat::fdat;

/// ONNX IR version 8 and operator set 17 (ONNX Runtime 1.14 and later).
const IR_VERSION: u64 = 8;
const OPSET: u64 = 17;
/// `TensorProto.DataType.FLOAT` and `INT64`.
const FLOAT: u64 = 1;
const INT64: u64 = 7;
/// The graph's input and output names.
pub const INPUT: &str = "input";
pub const OUTPUT: &str = "output";

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
    fn float(&mut self, field: u64, v: f32) -> &mut Self {
        self.key(field, 5);
        self.0.extend_from_slice(&v.to_le_bytes());
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
    Float(&'a str, f32),
    Str(&'a str, &'a str),
}

impl Attr<'_> {
    fn encode(&self) -> Msg {
        let mut m = Msg::default();
        match self {
            // AttributeType: FLOAT 1, INT 2, STRING 3, INTS 7 (field 20).
            Self::Float(n, v) => m.str(1, n).float(2, *v).int(20, 1),
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
    fn node(&mut self, op: &str, inputs: &[&str], attrs: &[Attr<'_>]) -> String {
        let out = self.fresh(&op.to_ascii_lowercase());
        self.node_to(op, inputs, attrs, out)
    }
    fn node_to(&mut self, op: &str, inputs: &[&str], attrs: &[Attr<'_>], out: String) -> String {
        let mut m = Msg::default();
        for i in inputs {
            m.str(1, i);
        }
        m.str(2, &out).str(3, &out).str(4, op);
        for a in attrs {
            m.msg(5, &a.encode());
        }
        self.nodes.push(m);
        out
    }
    /// An fp32 initializer of this shape (`[]`: a scalar).
    fn init(&mut self, p: &str, dims: &[usize], data: &[f32]) -> String {
        let raw: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.raw_init(p, dims, FLOAT, &raw)
    }
    /// A one-dimensional INT64 initializer (shapes, pads, slice bounds, axes).
    fn ints(&mut self, p: &str, data: &[i64]) -> String {
        let raw: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.raw_init(p, &[data.len()], INT64, &raw)
    }
    fn raw_init(&mut self, p: &str, dims: &[usize], ty: u64, raw: &[u8]) -> String {
        let name = self.fresh(p);
        let mut m = Msg::default();
        for d in dims {
            m.int(1, *d as u64);
        }
        m.int(2, ty).str(8, &name);
        m.bytes(9, raw);
        self.inits.push(m);
        name
    }
    /// The finished `ModelProto`: this graph between `input` and `output`.
    fn model(&self, name: &str, input: &Msg, output: &Msg) -> Vec<u8> {
        let mut graph = Msg::default();
        for n in &self.nodes {
            graph.msg(1, n);
        }
        graph.str(2, name);
        for i in &self.inits {
            graph.msg(5, i);
        }
        graph.msg(11, input).msg(12, output);
        let mut opset = Msg::default();
        opset.str(1, "").int(2, OPSET);
        let mut model = Msg::default();
        model.int(1, IR_VERSION).str(2, "capishot").msg(7, &graph).msg(8, &opset);
        model.0
    }
    /// A 3×3 convolution, padding 1 (into `out` when named).
    fn conv_to(&mut self, x: &str, c: &HostConv, out: Option<&str>) -> String {
        let w = self.init("w", &c.shape, &c.w);
        let b = self.init("b", &c.shape[..1], &c.b);
        let out = out.map_or_else(|| self.fresh("conv"), str::to_string);
        self.node_to("Conv", &[x, &w, &b], &[Attr::Ints("kernel_shape", &[3, 3]), Attr::Ints("pads", &[1, 1, 1, 1])], out)
    }
    fn conv(&mut self, x: &str, c: &HostConv) -> String {
        self.conv_to(x, c, None)
    }
    fn lrelu(&mut self, x: &str) -> String {
        self.node("LeakyRelu", &[x], &[Attr::Float("alpha", 0.2)])
    }
}

/// A `ValueInfoProto` for a `[1, channels, H, W]` fp32 tensor with free `H`, `W`.
fn image_value(name: &str, channels: usize, dims: [&str; 2]) -> Msg {
    value(name, &[Dim::Fixed(1), Dim::Fixed(channels), Dim::Free(dims[0]), Dim::Free(dims[1])])
}

/// One dimension of a [`value`]: a number, or a name ONNX Runtime binds at run time.
enum Dim<'a> {
    Fixed(usize),
    Free(&'a str),
}

/// A `ValueInfoProto` for an fp32 tensor of these dimensions.
fn value(name: &str, dims: &[Dim<'_>]) -> Msg {
    let mut shape = Msg::default();
    for d in dims {
        let mut dim = Msg::default();
        match d {
            Dim::Fixed(n) => dim.int(1, *n as u64),
            Dim::Free(s) => dim.str(2, s),
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

/// A first convolution's weights (`[out, ch·r², kh, kw]`, `w`) reordered for ONNX's
/// `SpaceToDepth`, which orders channels position-major ((dy·r + dx)·C + c), from PyTorch's
/// `pixel_unshuffle` order, channel-major (c·r² + dy·r + dx).
fn space_to_depth_order(w: &[f32], shape: [usize; 4], ch: usize, r: usize) -> Vec<f32> {
    let r2 = r * r;
    let [o, i, kh, kw] = shape;
    let k = kh * kw;
    let mut out = vec![0.0; w.len()];
    for oc in 0..o {
        for pos in 0..r2 {
            for ic in 0..ch {
                let (dst, src) = ((oc * i + pos * ch + ic) * k, (oc * i + ic * r2 + pos) * k);
                if let (Some(d), Some(s)) = (out.get_mut(dst..dst + k), w.get(src..src + k)) {
                    d.copy_from_slice(s);
                }
            }
        }
    }
    out
}

/// The network as ONNX model bytes: `input` `[1, image_in, H, W]` (sides multiples of the
/// unshuffle factor) → `output` `[1, out_nc, H·scale, W·scale]`, fp32 throughout.
pub(crate) fn esrgan(h: &HostEsrgan) -> Vec<u8> {
    let c = h.config;
    let mut g = Graph::default();
    let mut x = INPUT.to_string();
    let mut first = h.conv_first.clone();
    if c.shuffle > 1 {
        first.w = space_to_depth_order(&first.w, first.shape, h.image_in, c.shuffle);
        x = g.node("SpaceToDepth", &[&x], &[Attr::Int("blocksize", c.shuffle as i64)]);
    }
    let fea = g.conv(&x, &first);
    let res = g.init("res", &[], &[0.2]);
    let mut t = fea.clone();
    for block in &h.body {
        let block_in = t.clone();
        for rdb in block {
            let x0 = t.clone();
            let mut feats = vec![x0.clone()];
            for (j, conv) in rdb.iter().enumerate() {
                let refs: Vec<&str> = feats.iter().map(String::as_str).collect();
                let inp = if refs.len() == 1 { x0.clone() } else { g.node("Concat", &refs, &[Attr::Int("axis", 1)]) };
                let y = g.conv(&inp, conv);
                if j < 4 {
                    let y = g.lrelu(&y);
                    feats.push(y);
                } else {
                    let scaled = g.node("Mul", &[&y, &res], &[]);
                    t = g.node("Add", &[&scaled, &x0], &[]);
                }
            }
        }
        let scaled = g.node("Mul", &[&t, &res], &[]);
        t = g.node("Add", &[&scaled, &block_in], &[]);
    }
    let body = g.conv(&t, &h.conv_body);
    let mut fea = g.node("Add", &[&fea, &body], &[]);
    let scales = g.init("scales", &[4], &[1.0, 1.0, 2.0, 2.0]);
    for conv in [&h.conv_up1, &h.conv_up2] {
        let up = g.node(
            "Resize",
            &[&fea, "", &scales],
            &[Attr::Str("mode", "nearest"), Attr::Str("nearest_mode", "floor"), Attr::Str("coordinate_transformation_mode", "asymmetric")],
        );
        let y = g.conv(&up, conv);
        fea = g.lrelu(&y);
    }
    let hr = g.conv(&fea, &h.conv_hr);
    let hr = g.lrelu(&hr);
    g.conv_to(&hr, &h.conv_last, Some(OUTPUT));
    g.model("esrgan", &image_value(INPUT, h.image_in, ["h", "w"]), &image_value(OUTPUT, c.out_nc, ["oh", "ow"]))
}

/// Real-ESRGAN Compact as ONNX model bytes: `input` `[1, channels, H, W]` → `output`
/// `[1, channels, H·scale, W·scale]`, fp32, the graph of [`crate::compact::Compact`]'s forward:
/// the convs and PReLUs, pixel shuffle (`DepthToSpace`, PyTorch's order), plus the
/// nearest-upsampled input; the ×2 variant averages each 2 × 2 block of the ×4 result.
pub(crate) fn compact(h: &HostCompact) -> Vec<u8> {
    let r = h.config.upscale;
    let mut g = Graph::default();
    let mut t = INPUT.to_string();
    for (conv, alpha) in &h.layers {
        t = g.conv(&t, conv);
        if let Some(a) = alpha {
            let slope = g.init("a", &[a.len(), 1, 1], a);
            t = g.node("PRelu", &[&t, &slope], &[]);
        }
    }
    let y = g.node("DepthToSpace", &[&t], &[Attr::Int("blocksize", r as i64), Attr::Str("mode", "CRD")]);
    let scales = g.init("scales", &[4], &[1.0, 1.0, r as f32, r as f32]);
    let base = g.node(
        "Resize",
        &[INPUT, "", &scales],
        &[Attr::Str("mode", "nearest"), Attr::Str("nearest_mode", "floor"), Attr::Str("coordinate_transformation_mode", "asymmetric")],
    );
    if h.scale == r {
        g.node_to("Add", &[&y, &base], &[], OUTPUT.into());
    } else {
        let sum = g.node("Add", &[&y, &base], &[]);
        g.node_to("AveragePool", &[&sum], &[Attr::Ints("kernel_shape", &[2, 2]), Attr::Ints("strides", &[2, 2])], OUTPUT.into());
    }
    g.model("compact", &image_value(INPUT, h.channels, ["h", "w"]), &image_value(OUTPUT, h.channels, ["oh", "ow"]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_and_keys() {
        let mut m = Msg::default();
        m.varint(1);
        m.varint(300);
        m.int(20, 7);
        assert_eq!(m.0, [0x01, 0xac, 0x02, 0xa0, 0x01, 0x07]);
    }

    #[test]
    fn tiny_esrgan_encodes_every_conv() {
        let w = crate::esrgan::tests::tiny(12, "2");
        let h = crate::esrgan::Esrgan::host(&w, true).unwrap();
        let bytes = esrgan(&h);
        let text = String::from_utf8_lossy(&bytes);
        // conv_first, 3 dense blocks of 5 per RRDB block, conv_body and the 4 tail convs.
        assert_eq!(text.matches("Conv").count(), 1 + h.body.len() * 15 + 5);
        assert!(text.contains("SpaceToDepth") && text.contains(OUTPUT));
        // Every weight is in there as raw fp32.
        let weights: usize = h.body.iter().flatten().flatten().map(|c| c.w.len() + c.b.len()).sum();
        assert!(bytes.len() > weights * 4);
    }

    #[test]
    fn int64_initializers_and_fixed_dims() {
        let mut g = Graph::default();
        let name = g.ints("s", &[-1, 2]);
        assert_eq!(name, "s1");
        // dims [2] (field 1), INT64 (field 2 = 7), name, raw_data: two little-endian i64s.
        let mut want = vec![0x08, 0x02, 0x10, 0x07, 0x42, 0x02, b's', b'1', 0x4a, 16];
        want.extend_from_slice(&(-1i64).to_le_bytes());
        want.extend_from_slice(&2i64.to_le_bytes());
        assert_eq!(g.inits[0].0, want);
        // A fixed dimension is dim_value (field 1), a free one dim_param (field 2).
        let v = value("x", &[Dim::Fixed(3), Dim::Free("h")]);
        let text = String::from_utf8_lossy(&v.0);
        assert!(text.contains('x') && text.contains('h'));
        assert!(v.0.windows(2).any(|w| w == [0x08, 0x03]));
    }

    #[test]
    fn space_to_depth_reorders_channel_major_inputs() {
        // One output, 2 channels unshuffled by 2 (8 inputs), 1×1 kernel: input c·4 + pos moves to
        // pos·2 + c.
        let w: Vec<f32> = (0..8).map(|i| i as f32).collect();
        assert_eq!(space_to_depth_order(&w, [1, 8, 1, 1], 2, 2), [0.0, 4.0, 1.0, 5.0, 2.0, 6.0, 3.0, 7.0]);
    }

    #[test]
    fn tiny_fdat_graph() {
        let count = |b: &[u8], op: &str| String::from_utf8_lossy(b).matches(op).count();
        for shuffle in [1, 2] {
            let w = crate::directml::tests::tiny_fdat(shuffle);
            // 22 × 14 at ×4 (11 × 7 at ×2): window padding and its crop in the spatial block.
            let bytes = fdat(&w, (22, 14)).unwrap();
            assert_eq!(count(&bytes, "LayerNormalization"), 4);
            assert_eq!(count(&bytes, "ConvTranspose"), 2);
            assert_eq!((count(&bytes, "Pad"), count(&bytes, "Slice")), (1, 1));
            assert_eq!(count(&bytes, "SpaceToDepth"), usize::from(shuffle > 1));
            assert_eq!(count(&bytes, "ReduceL2"), 2);
            assert_eq!(count(&bytes, "Erf"), 6);
            // Multiples of the window: no padding.
            let bytes = fdat(&w, (16 * shuffle, 8 * shuffle)).unwrap();
            assert_eq!((count(&bytes, "Pad"), count(&bytes, "Slice")), (0, 0));
        }
        let w = crate::directml::tests::tiny_fdat(2);
        assert!(fdat(&w, (15, 16)).is_err() && fdat(&w, (0, 16)).is_err() && fdat(&w, (1 << 20, 16)).is_err());
        assert!(fdat(&crate::weights::Weights::default(), (16, 16)).is_err());
    }
}
