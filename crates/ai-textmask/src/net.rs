//! The mask model on burn, for the browser: a ResNet-18 encoder, an ASPP and a U-Net decoder, as
//! `manga-text-mask-precise.onnx` has them (`packaging/web-ai/convert-textmask.py` writes the weights).
//! Same results as the ONNX file (`examples/netcheck.rs`).

use burn::tensor::activation::relu;
use burn::tensor::module::{conv2d, interpolate, max_pool2d};
use burn::tensor::ops::{ConvOptions, InterpolateMode, InterpolateOptions, MaxPoolOptions};
use burn::tensor::{Device, Tensor, TensorData};
use safetensors::{Dtype, SafeTensors};

use crate::regions::Detection;
use crate::{Error, pipeline};

const CONVS: usize = 37;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
/// The convolutions that go down a size.
const STRIDE_2: [usize; 7] = [0, 5, 7, 10, 12, 15, 17];

#[derive(Debug, Clone)]
struct Conv {
    w: Tensor<4>,
    b: Option<Tensor<1>>,
    stride: usize,
    padding: usize,
    dilation: usize,
}

impl Conv {
    fn run(&self, x: Tensor<4>) -> Tensor<4> {
        conv2d(x, self.w.clone(), self.b.clone(), ConvOptions::new([self.stride; 2], [self.padding; 2], [self.dilation; 2], 1))
    }
}

/// The loaded network, on one device.
#[derive(Debug, Clone)]
pub struct Net {
    device: Device,
    c: Vec<Conv>,
}

fn floats(view: &safetensors::tensor::TensorView<'_>) -> Result<Vec<f32>, Error> {
    let d = view.data();
    match view.dtype() {
        Dtype::F16 => Ok(d.chunks_exact(2).map(|b| half::f16::from_le_bytes([b[0], b[1]]).to_f32()).collect()),
        Dtype::F32 => Ok(d.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()),
        other => Err(Error::Unavailable(format!("unexpected weight type {other:?}"))),
    }
}

impl Net {
    pub fn from_safetensors(bytes: &[u8], device: &Device) -> Result<Self, Error> {
        let bad = |e: String| Error::Unavailable(format!("bad text model file: {e}"));
        let st = SafeTensors::deserialize(bytes).map_err(|e| bad(e.to_string()))?;
        let mut c = Vec::with_capacity(CONVS);
        for k in 0..CONVS {
            let w = st.tensor(&format!("c{k:02}.w")).map_err(|e| bad(e.to_string()))?;
            let [o, i, kh, kw] = <[usize; 4]>::try_from(w.shape()).map_err(|_| bad(format!("c{k:02} isn't a convolution")))?;
            let b = match st.tensor(&format!("c{k:02}.b")) {
                Ok(v) => Some(Tensor::<1>::from_data(TensorData::new(floats(&v)?, [o]), device)),
                Err(_) => None,
            };
            let dilation = match k {
                21 => 2,
                22 => 4,
                23 => 8,
                _ => 1,
            };
            c.push(Conv {
                w: Tensor::<4>::from_data(TensorData::new(floats(&w)?, [o, i, kh, kw]), device),
                b,
                stride: if STRIDE_2.contains(&k) { 2 } else { 1 },
                padding: kh / 2 * dilation,
                dilation,
            });
        }
        Ok(Net { device: device.clone(), c })
    }

    /// Logits `[1, 1, h, w]` of planar `x` in 0..=1, both sides multiples of 32.
    fn forward(&self, x: Vec<f32>, w: usize, h: usize) -> Tensor<4> {
        let c = &self.c;
        let d = &self.device;
        let mean = Tensor::<4>::from_data(TensorData::new(MEAN.to_vec(), [1, 3, 1, 1]), d);
        let std = Tensor::<4>::from_data(TensorData::new(STD.to_vec(), [1, 3, 1, 1]), d);
        let input = (Tensor::<4>::from_data(TensorData::new(x, [1, 3, h, w]), d) - mean) / std;
        let r0 = relu(c[0].run(input.clone()));
        let mut pool = MaxPoolOptions::new([3, 3]);
        pool.stride = [2, 2];
        pool.padding = [(1, 1); 2];
        let p = max_pool2d(r0.clone(), pool);
        // ResNet-18's first layer keeps the size; the others start with a stride and a shortcut.
        let r2 = relu(c[2].run(relu(c[1].run(p.clone()))) + p);
        let l1 = relu(c[4].run(relu(c[3].run(r2.clone()))) + r2);
        let down = |at: usize, x: &Tensor<4>| -> Tensor<4> {
            let a = relu(c[at + 1].run(relu(c[at].run(x.clone()))) + c[at + 2].run(x.clone()));
            relu(c[at + 4].run(relu(c[at + 3].run(a.clone()))) + a)
        };
        let l2 = down(5, &l1);
        let l3 = down(10, &l2);
        let l4 = down(15, &l3);
        let [_, _, lh, lw] = l4.dims();
        // ASPP: the global mean and three dilations.
        let mean = l4.clone().mean_dim(2).mean_dim(3);
        let global = relu(c[20].run(mean)).expand([1, 128, lh, lw]);
        let aspp = relu(c[24].run(Tensor::cat(
            vec![l4.clone(), global, relu(c[21].run(l4.clone())), relu(c[22].run(l4.clone())), relu(c[23].run(l4))],
            1,
        )));
        let up = |x: Tensor<4>, size: [usize; 2]| interpolate(x, InterpolateOptions::new(InterpolateMode::Bilinear).with_align_corners(false).with_output_size(size));
        let block = |at: usize, x: Tensor<4>, skip: Tensor<4>, k: usize| -> Tensor<4> {
            let x = Tensor::cat(vec![up(x, [lh * k, lw * k]), skip], 1);
            relu(c[at + 1].run(relu(c[at].run(x))))
        };
        let x = block(25, aspp, l3, 2);
        let x = block(27, x, l2, 4);
        let x = block(29, x, l1, 8);
        let x = block(31, x, r0, 16);
        let x = up(x, [h, w]);
        let fine = relu(c[34].run(relu(c[33].run(input))));
        c[36].run(relu(c[35].run(Tensor::cat(vec![x, fine], 1))))
    }

    /// A tile's logits, reading back without blocking (WebGPU).
    pub async fn logits_async(&self, rgb: &[u8], t: usize) -> Result<Vec<f32>, Error> {
        let out = self.forward(pipeline::planar(rgb, t, t), t, t);
        let data = out.into_data_async().await.map_err(|e| Error::Device(format!("{e:?}")))?;
        data.convert::<f32>().into_vec::<f32>().map_err(|e| Error::Device(format!("{e:?}")))
    }

    /// The page's regions, in tiles of `t` overlapping by `overlap`. `progress` gets 0..=1.
    pub async fn detect_async(&self, rgb: &[u8], w: usize, h: usize, t: usize, overlap: usize, progress: &mut dyn FnMut(f32)) -> Result<Detection, Error> {
        if w == 0 || h == 0 || rgb.len() != w * h * 3 {
            return Err(Error::Input("the page's buffer doesn't match its size".into()));
        }
        let (nw, nh) = pipeline::net_size(w, h);
        let resized = ((nw, nh) != (w, h)).then(|| crate::resize::rgb(rgb, w, h, nw, nh));
        let page = resized.as_deref().unwrap_or(rgb);
        let mut blend = pipeline::Blend::new(nw, nh, t, overlap);
        let (ys, xs) = (pipeline::starts(nh, t, overlap), pipeline::starts(nw, t, overlap));
        let total = (ys.len() * xs.len()) as f32;
        let mut done = 0.0;
        for &y0 in &ys {
            for &x0 in &xs {
                let logits = self.logits_async(&blend.tile(page, x0, y0), t).await?;
                blend.add(&logits, x0, y0);
                done += 1.0;
                progress(done / total);
            }
        }
        Ok(pipeline::regions(&blend.logits(), nw, nh, rgb, w, h))
    }
}
