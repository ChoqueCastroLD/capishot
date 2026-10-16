//! Loading a tier's files and running one Generative Fill request end to end, shared by the
//! native runner and the browser's AI worker.
//!
//! Files are added one at a time ([`Loader::add`], any order; the file's `arch` metadata says
//! what it is), so the worker can stream them from Cache Storage and drop each one's bytes once
//! its tensors are on the device. [`generate`] is `async` (WebGPU read-back); natively callers
//! block on it.

use burn::tensor::{DType, Device, Tensor};

use crate::clip::TextEncoder;
use crate::nn::Store;
use crate::pipeline::{Decoder, Encoder, Generation, Request, decode};
use crate::scheduler::Sampler;
use crate::taesd::{TinyDecoder, TinyEncoder};
use crate::tokenizer::Tokenizer;
use crate::unet::Unet;
use crate::vae::{VaeDecoder, VaeEncoder};
use crate::{Result, SdError};

/// What a weight file holds (its `arch` metadata).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Text,
    UnetShard,
    Lora,
    VaeEncoder,
    VaeDecoder,
    Taesd,
}

/// The part a file holds, from its `arch` metadata.
pub fn part_from_arch(arch: Option<&str>) -> Result<Part> {
    match arch {
        Some("clip-text-l14") => Ok(Part::Text),
        Some("sd15-unet") => Ok(Part::UnetShard),
        Some("sd15-unet-lora") => Ok(Part::Lora),
        Some("sd-vae-encoder") => Ok(Part::VaeEncoder),
        Some("sd-vae-decoder") => Ok(Part::VaeDecoder),
        Some("taesd") => Ok(Part::Taesd),
        other => Err(SdError::Weights(format!("not a Generative Fill file (arch {other:?})"))),
    }
}

/// The compute precision for a device: f16 where the device has it (WebGPU `shader-f16`,
/// Vulkan/Metal/DX12), f32 on the CPU.
pub fn precision(device: &Device, cpu: bool) -> DType {
    if !cpu && device.supports_dtype(DType::F16) { DType::F16 } else { DType::F32 }
}

/// One file being loaded (see [`Loader::open`]).
pub struct FileSink {
    part: Part,
    store: Store,
    merges: Option<Vec<u8>>,
}

impl FileSink {
    pub fn part(&self) -> Part {
        self.part
    }

    /// Uploads one streamed tensor.
    pub fn tensor(&mut self, t: &crate::stream::RawTensor) -> Result<()> {
        if t.name == "tokenizer.merges" {
            self.merges = Some(t.data.clone());
            return Ok(());
        }
        self.store.add_raw(t)
    }
}

/// Collects files, then builds [`Models`].
pub struct Loader {
    device: Device,
    dtype: DType,
    text: Option<(TextEncoder, Tokenizer)>,
    unet: Store,
    lora: Option<Store>,
    vae_enc: Option<VaeEncoder>,
    vae_dec: Option<VaeDecoder>,
    taesd: Option<(TinyEncoder, TinyDecoder)>,
}

impl Loader {
    pub fn new(device: &Device, dtype: DType) -> Result<Self> {
        Ok(Self { device: device.clone(), dtype, text: None, unet: Store::new(device, dtype)?, lora: None, vae_enc: None, vae_dec: None, taesd: None })
    }

    /// Uploads one file's tensors (its bytes can be dropped afterwards). Natively the whole file
    /// is in memory; streamed files go through [`Loader::open`] instead.
    pub fn add(&mut self, bytes: &[u8]) -> Result<Part> {
        let mut parser = crate::stream::StreamParser::new();
        let mut sink: Option<FileSink> = None;
        for chunk in bytes.chunks(16 << 20) {
            let ts = parser.push(chunk)?;
            if sink.is_none() && !parser.metadata().is_empty() {
                sink = Some(self.open(parser.metadata())?);
            }
            if let Some(s) = sink.as_mut() {
                for t in &ts {
                    s.tensor(t)?;
                }
            }
        }
        if !parser.finished() {
            return Err(SdError::Weights("truncated safetensors file".into()));
        }
        let sink = sink.ok_or_else(|| SdError::Weights("not a Generative Fill file (no metadata)".into()))?;
        self.close(sink)
    }

    /// Starts a file from its header's metadata (see [`crate::stream::StreamParser::metadata`]).
    pub fn open(&self, metadata: &std::collections::HashMap<String, String>) -> Result<FileSink> {
        let part = part_from_arch(metadata.get("arch").map(String::as_str))?;
        Ok(FileSink { part, store: Store::new(&self.device, self.dtype)?, merges: None })
    }

    /// Finishes a file: builds its network (or keeps the UNet shard / LoRA for [`Loader::finish`]).
    pub fn close(&mut self, f: FileSink) -> Result<Part> {
        let FileSink { part, store, merges } = f;
        match part {
            Part::Text => {
                let merges = merges.ok_or_else(|| SdError::Weights("the text encoder file has no tokenizer".into()))?;
                let merges = String::from_utf8(merges).map_err(|_| SdError::Weights("tokenizer merges aren't UTF-8".into()))?;
                let tok = Tokenizer::from_merges(&merges).map_err(|e| SdError::Weights(e.to_string()))?;
                self.text = Some((TextEncoder::from_store(&store)?, tok));
            }
            Part::UnetShard => self.unet.absorb(store),
            Part::Lora => self.lora = Some(store),
            Part::VaeEncoder => self.vae_enc = Some(VaeEncoder::from_store(&store)?),
            Part::VaeDecoder => self.vae_dec = Some(VaeDecoder::from_store(&store)?),
            Part::Taesd => self.taesd = Some((TinyEncoder::from_store(&store)?, TinyDecoder::from_store(&store)?)),
        }
        Ok(part)
    }

    /// Fuses the LoRA (if any) and builds the UNet.
    pub fn finish(mut self) -> Result<Models> {
        let (text, tokenizer) = self.text.ok_or_else(|| SdError::Weights("the text encoder file is missing".into()))?;
        let lora = match self.lora.take() {
            Some(l) => {
                crate::lora::fuse(&mut self.unet, &l, 1.0)?;
                true
            }
            None => false,
        };
        let unet = Unet::from_store(&self.unet)?;
        let scratch = Store::new(&self.device, self.dtype)?;
        Ok(Models { text, tokenizer, unet, lora, vae_enc: self.vae_enc, vae_dec: self.vae_dec, taesd: self.taesd, scratch })
    }
}

/// A loaded tier.
pub struct Models {
    pub text: TextEncoder,
    pub tokenizer: Tokenizer,
    pub unet: Unet,
    /// A few-step LoRA was fused.
    pub lora: bool,
    pub vae_enc: Option<VaeEncoder>,
    pub vae_dec: Option<VaeDecoder>,
    pub taesd: Option<(TinyEncoder, TinyDecoder)>,
    /// Creates input tensors (device and precision).
    pub scratch: Store,
}

/// One run's settings (the working-size crop, prompts already in English).
#[derive(Clone, Debug)]
pub struct Params {
    pub prompt: String,
    pub negative: String,
    pub rgb: Vec<f32>,
    pub mask: Vec<f32>,
    pub width: usize,
    pub height: usize,
    pub variants: usize,
    pub seed: u64,
    pub steps: usize,
    pub strength: f64,
    pub guidance: f32,
    pub sampler: Sampler,
    /// Encode/decode with TAESD even when the full VAE is loaded.
    pub tiny_vae: bool,
}

/// Generates `variants` images (interleaved RGB 0..=1 at the working size). `step(done, total)`
/// is called after every UNet step (the device has finished it) and returns `false` to cancel.
pub async fn generate(m: &Models, p: &Params, step: &mut dyn FnMut(usize, usize) -> bool) -> Result<Vec<Vec<f32>>> {
    let (prompt, truncated) = m.tokenizer.encode_padded(&p.prompt);
    let _ = truncated;
    let negative = if p.guidance > 1.0 { Some(m.tokenizer.encode_padded(&p.negative).0) } else { None };
    let req = Request {
        prompt,
        negative,
        guidance: p.guidance,
        rgb: p.rgb.clone(),
        mask: p.mask.clone(),
        width: p.width,
        height: p.height,
        variants: p.variants,
        seed: p.seed,
        steps: p.steps,
        strength: p.strength,
        sampler: p.sampler,
    };
    let tiny = p.tiny_vae || m.vae_enc.is_none() || m.vae_dec.is_none();
    let encoder = match (&m.vae_enc, &m.taesd) {
        (Some(e), _) if !tiny => Encoder::Full(e),
        (_, Some((e, _))) => Encoder::Tiny(e),
        (Some(e), None) => Encoder::Full(e),
        (None, None) => return Err(SdError::Weights("no image encoder loaded".into())),
    };
    let mut g = Generation::start(&m.scratch, &m.text, &m.unet, encoder, &req)?;
    let (_, total) = g.progress();
    while !g.done() {
        g.step(&m.scratch, &m.unet)?;
        // Wait for the device so progress is real (a one-value read-back).
        sync(g.latents()).await?;
        if !step(g.progress().0, total) {
            return Err(SdError::Cancelled);
        }
    }
    let decoder = match (&m.vae_dec, &m.taesd) {
        (Some(d), _) if !tiny => Decoder::Full(d),
        (_, Some((_, d))) => Decoder::Tiny(d),
        (Some(d), None) => Decoder::Full(d),
        (None, None) => return Err(SdError::Weights("no image decoder loaded".into())),
    };
    let img = decode(decoder, g.latents())?;
    let [b, c, h, w] = img.dims();
    let data = img.cast(DType::F32).permute([0, 2, 3, 1]).into_data_async().await.map_err(|e| SdError::Device(format!("{e:?}")))?;
    let v = data.try_into_vec::<f32>().map_err(|e| SdError::Device(format!("read-back: {e:?}")))?;
    let n = h * w * c;
    if v.len() != b * n {
        return Err(SdError::Device(format!("read back {} values, expected {}", v.len(), b * n)));
    }
    Ok(v.chunks_exact(n).map(<[f32]>::to_vec).collect())
}

async fn sync(t: Tensor<4>) -> Result<()> {
    let one = t.slice([0..1, 0..1, 0..1, 0..1]).cast(DType::F32);
    one.into_data_async().await.map(|_| ()).map_err(|e| SdError::Device(format!("{e:?}")))
}

/// The runner's parameters for a request on loaded models (tier defaults filled in).
pub fn params(req: &crate::service::Request, m: &Models) -> Params {
    let steps = if req.steps == 0 { req.tier.steps() } else { req.steps };
    let guidance = if req.guidance > 0.0 {
        req.guidance
    } else if m.lora {
        1.0
    } else {
        7.0
    };
    Params {
        prompt: req.prompt.clone(),
        negative: req.negative.clone(),
        rgb: req.rgb.clone(),
        mask: req.mask.clone(),
        width: req.width,
        height: req.height,
        variants: req.variants,
        seed: req.seed,
        steps,
        strength: req.strength,
        guidance,
        sampler: crate::files::SAMPLER,
        tiny_vae: matches!(req.tier, crate::service::Tier::Ultra | crate::service::Tier::Fast),
    }
}
