//! ESRGAN as an ONNX model, written from its weights in memory (for the DirectML backend,
//! `crate::directml`): the same graph as [`crate::esrgan::Esrgan::forward`] in fp32, with the
//! grayscale folding already in the weights. Nothing is stored or downloaded: the model bytes
//! are built when the network loads and handed to ONNX Runtime.
//!
//! The encoder writes only the few protobuf messages an inference graph needs (`ModelProto`,
//! `GraphProto`, `NodeProto`, `AttributeProto`, `TensorProto`, `ValueInfoProto`), by field
//! number as `onnx.proto` defines them.

use crate::esrgan::{HostConv, HostEsrgan};

/// ONNX IR version 8 and operator set 17 (ONNX Runtime 1.14 and later).
const IR_VERSION: u64 = 8;
const OPSET: u64 = 17;
/// `TensorProto.DataType.FLOAT`.
const FLOAT: u64 = 1;
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

/// A graph being built: nodes, fp32 initializers, fresh value names.
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
        let name = self.fresh(p);
        let mut m = Msg::default();
        for d in dims {
            m.int(1, *d as u64);
        }
        m.int(2, FLOAT).str(8, &name);
        let raw: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
        m.bytes(9, &raw);
        self.inits.push(m);
        name
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
    let mut shape = Msg::default();
    for d in ["1", "c", dims[0], dims[1]] {
        let mut dim = Msg::default();
        match d {
            "1" => dim.int(1, 1),
            "c" => dim.int(1, channels as u64),
            _ => dim.str(2, d),
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

/// The network as ONNX model bytes: `input` `[1, image_in, H, W]` (sides multiples of the
/// unshuffle factor) → `output` `[1, out_nc, H·scale, W·scale]`, fp32 throughout.
pub(crate) fn esrgan(h: &HostEsrgan) -> Vec<u8> {
    let c = h.config;
    let mut g = Graph::default();
    let mut x = INPUT.to_string();
    let mut first = h.conv_first.clone();
    if c.shuffle > 1 {
        // ONNX's SpaceToDepth orders channels position-major ((dy·r + dx)·C + c), PyTorch's
        // pixel_unshuffle channel-major (c·r² + dy·r + dx): permute conv_first's inputs to match.
        let (r2, ch) = (c.shuffle * c.shuffle, h.image_in);
        let [o, i, kh, kw] = first.shape;
        let k = kh * kw;
        let mut w = vec![0.0; first.w.len()];
        for oc in 0..o {
            for pos in 0..r2 {
                for ic in 0..ch {
                    let (dst, src) = ((oc * i + pos * ch + ic) * k, (oc * i + ic * r2 + pos) * k);
                    if let (Some(d), Some(s)) = (w.get_mut(dst..dst + k), first.w.get(src..src + k)) {
                        d.copy_from_slice(s);
                    }
                }
            }
        }
        first.w = w;
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

    let mut graph = Msg::default();
    for n in &g.nodes {
        graph.msg(1, n);
    }
    graph.str(2, "esrgan");
    for i in &g.inits {
        graph.msg(5, i);
    }
    graph.msg(11, &image_value(INPUT, h.image_in, ["h", "w"]));
    graph.msg(12, &image_value(OUTPUT, c.out_nc, ["oh", "ow"]));
    let mut opset = Msg::default();
    opset.str(1, "").int(2, OPSET);
    let mut model = Msg::default();
    model.int(1, IR_VERSION).str(2, "capishot").msg(7, &graph).msg(8, &opset);
    model.0
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
}
