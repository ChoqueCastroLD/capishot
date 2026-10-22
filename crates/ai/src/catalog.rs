//! The models Capishot knows: what they are, where their weights come from, how big they are.
//!
//! Weight files are the original releases converted to fp16 safetensors with new-arch ESRGAN key
//! names (`conv_first`, `body.N.rdbM.convK`, `conv_body`, `conv_up1`, `conv_up2`, `conv_hr`,
//! `conv_last`); conversion is lossless (the releases are fp16). They are downloaded on first use
//! from [`ModelInfo::url`] under a base URL the shell configures, verified against
//! [`ModelInfo::sha256`] and cached.

/// The network architecture of a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Arch {
    /// ESRGAN RRDBNet (23 RRDB blocks, 64 filters). 2x models pixel-unshuffle their input.
    Esrgan,
    /// LaMa (big-lama, Fast Fourier Convolutions), run by `photocraft-ai-lama`.
    Lama,
    /// Real-ESRGAN Compact (SRVGGNetCompact): the lite upscaler ([`crate::compact`]).
    Compact,
    /// MI-GAN: the lite inpainter, run by `photocraft-ai-lama`.
    Migan,
    /// A Generative Fill file (Stable Diffusion 1.5 inpainting parts, TAESD, few-step LoRA), run
    /// by `photocraft-ai-sd`.
    Generative,
}

/// What a model is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Content {
    /// Black-and-white manga pages (MangaJaNai).
    Grayscale,
    /// Colour pages, covers and illustrations (IllustrationJaNai).
    Color,
    /// Inpainting: fills masked areas (text, SFX) with background and screentone.
    Inpaint,
    /// Upscaling of any page, grayscale or colour (the lite upscaler).
    Any,
    /// Generative Fill: creates new content from a prompt.
    Generative,
}

/// One downloadable model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// Stable id used in command params, caches and URLs.
    pub id: &'static str,
    /// Shown in menus.
    pub title: &'static str,
    /// The weight file name (under the models base URL).
    pub file: &'static str,
    pub arch: Arch,
    pub scale: u32,
    pub content: Content,
    /// The page height the model was trained for (MangaJaNai), if any.
    pub height: Option<u32>,
    /// Size of the weight file in bytes.
    pub bytes: u64,
    pub sha256: &'static str,
    /// SPDX licence of the weights.
    pub license: &'static str,
    pub author: &'static str,
    /// Where the original release lives.
    pub source: &'static str,
    /// A lite model: much less work than the full one for the same job, for phones, weak
    /// machines and CPUs (lower quality; the UI says so).
    pub lite: bool,
}

impl ModelInfo {
    /// The download URL under `base` (with or without a trailing slash).
    pub fn url(&self, base: &str) -> String {
        format!("{}/{}", base.trim_end_matches('/'), self.file)
    }
}

const MJ_SRC: &str = "https://github.com/the-database/MangaJaNai/releases/tag/1.0.0";
const LICENSE: &str = "CC-BY-NC-4.0";
const AUTHOR: &str = "the-database";
const B2: u64 = 33_472_214;
const B4: u64 = 33_461_846;

macro_rules! mj {
    ($id:literal, $title:literal, $file:literal, $scale:literal, $h:literal, $bytes:expr, $sha:literal) => {
        ModelInfo {
            id: $id,
            title: $title,
            file: $file,
            arch: Arch::Esrgan,
            scale: $scale,
            content: Content::Grayscale,
            height: Some($h),
            bytes: $bytes,
            sha256: $sha,
            license: LICENSE,
            author: AUTHOR,
            source: MJ_SRC,
            lite: false,
        }
    };
}

/// The lite upscaler: one weight file (`realesr-animevideov3`, the ×4 network) for both scales;
/// ×2 runs it and box-downsamples.
macro_rules! compact {
    ($id:expr, $title:literal, $scale:literal) => {
        ModelInfo {
            id: $id,
            title: $title,
            file: "realesr-animevideov3-f16.safetensors",
            arch: Arch::Compact,
            scale: $scale,
            content: Content::Any,
            height: None,
            bytes: 1_247_200,
            sha256: "a0b2c646562345be46efbb2f863c5f4f873c0c2b18a86186cebd3ec2bf3ff777",
            license: "BSD-3-Clause",
            author: "Xintao Wang (Real-ESRGAN)",
            source: "https://github.com/xinntao/Real-ESRGAN/releases/tag/v0.2.5.0",
            lite: true,
        }
    };
}

const DS8_SRC: &str = "https://huggingface.co/Lykon/dreamshaper-8-inpainting";
const OPENRAIL: &str = "CreativeML-OpenRAIL-M";
const DS8_AUTHOR: &str = "Lykon (DreamShaper 8) on Stable Diffusion 1.5 (CompVis, Runway, Stability AI)";

/// One Generative Fill file.
macro_rules! genfill_file {
    ($id:expr, $title:literal, $file:literal, $bytes:expr, $sha:literal, $license:expr, $author:expr, $source:expr) => {
        ModelInfo {
            id: $id,
            title: $title,
            file: $file,
            arch: Arch::Generative,
            scale: 1,
            content: Content::Generative,
            height: None,
            bytes: $bytes,
            sha256: $sha,
            license: $license,
            author: $author,
            source: $source,
            lite: false,
        }
    };
}

/// Every known model.
pub const MODELS: &[ModelInfo] = &[
    mj!(
        "mangajanai-v1-2x-1200p",
        "MangaJaNai 2x (1200p)",
        "2x_MangaJaNai_1200p_V1_ESRGAN_70k.safetensors",
        2,
        1200,
        B2,
        "77d3274d9e0bbf72f3675d0eaf1e8aea79b4124623a022603dda93f87d80a74f"
    ),
    mj!(
        "mangajanai-v1-2x-1300p",
        "MangaJaNai 2x (1300p)",
        "2x_MangaJaNai_1300p_V1_ESRGAN_75k.safetensors",
        2,
        1300,
        B2,
        "e440654f3ac435539fcccbac86cf2813b50ac533cf7c0dbeceaf61eefffc5e13"
    ),
    mj!(
        "mangajanai-v1-2x-1400p",
        "MangaJaNai 2x (1400p)",
        "2x_MangaJaNai_1400p_V1_ESRGAN_70k.safetensors",
        2,
        1400,
        B2,
        "717f0248d56ffe2f41f1cf40326c811cc187abb7e74e6c9477ae7ede90d9b2a6"
    ),
    mj!(
        "mangajanai-v1-2x-1500p",
        "MangaJaNai 2x (1500p)",
        "2x_MangaJaNai_1500p_V1_ESRGAN_90k.safetensors",
        2,
        1500,
        B2,
        "711bcc0b6b7d00bb068886695efeae0a7f14a41f7086833d92480f7819e75a7c"
    ),
    mj!(
        "mangajanai-v1-2x-1600p",
        "MangaJaNai 2x (1600p)",
        "2x_MangaJaNai_1600p_V1_ESRGAN_90k.safetensors",
        2,
        1600,
        B2,
        "d69ed784b0296792cd0a8876cf4a8162e7fe714ade0d061a7eb6ecf8a3047273"
    ),
    mj!(
        "mangajanai-v1-2x-1920p",
        "MangaJaNai 2x (1920p)",
        "2x_MangaJaNai_1920p_V1_ESRGAN_70k.safetensors",
        2,
        1920,
        B2,
        "5f8af7ce3caa2b8db60ad8f80248868568aa34969bd76c2626cef39a6764f86a"
    ),
    mj!(
        "mangajanai-v1-2x-2048p",
        "MangaJaNai 2x (2048p)",
        "2x_MangaJaNai_2048p_V1_ESRGAN_95k.safetensors",
        2,
        2048,
        B2,
        "3c03f46781ae68230a13e731f07d98216b0ff9354f04d4ff0ef11e7e434a55d6"
    ),
    mj!(
        "mangajanai-v1-4x-1200p",
        "MangaJaNai 4x (1200p)",
        "4x_MangaJaNai_1200p_V1_ESRGAN_70k.safetensors",
        4,
        1200,
        B4,
        "393dd063f4fca32c226439a3ac664eaec9154a230094e0a526f31111db6770e7"
    ),
    mj!(
        "mangajanai-v1-4x-1300p",
        "MangaJaNai 4x (1300p)",
        "4x_MangaJaNai_1300p_V1_ESRGAN_75k.safetensors",
        4,
        1300,
        B4,
        "8d1cb08e0a4c20e3521cdd0709e9e2ddaf7866ef11a997b63f6f422f2171fe9d"
    ),
    mj!(
        "mangajanai-v1-4x-1400p",
        "MangaJaNai 4x (1400p)",
        "4x_MangaJaNai_1400p_V1_ESRGAN_105k.safetensors",
        4,
        1400,
        B4,
        "5709956bc9cd5574a470f8cb6c4bdfd3c60dc63b32b30ca73a3b332d4f3d0fe1"
    ),
    mj!(
        "mangajanai-v1-4x-1500p",
        "MangaJaNai 4x (1500p)",
        "4x_MangaJaNai_1500p_V1_ESRGAN_105k.safetensors",
        4,
        1500,
        B4,
        "9802b24ed638b60427e9a6ce3b013b0f4235dedfdc6d4408fbe83a14a20383fe"
    ),
    mj!(
        "mangajanai-v1-4x-1600p",
        "MangaJaNai 4x (1600p)",
        "4x_MangaJaNai_1600p_V1_ESRGAN_70k.safetensors",
        4,
        1600,
        B4,
        "c3d4f844e61fec52ac229ff3ee6ed2b0ec3bdaed5d89e6fbb9675d9a04a87580"
    ),
    mj!(
        "mangajanai-v1-4x-1920p",
        "MangaJaNai 4x (1920p)",
        "4x_MangaJaNai_1920p_V1_ESRGAN_105k.safetensors",
        4,
        1920,
        B4,
        "07018e68fa9bc52d9bd9be1bc7ab5247b62134058118074286169790f7fd2e95"
    ),
    mj!(
        "mangajanai-v1-4x-2048p",
        "MangaJaNai 4x (2048p)",
        "4x_MangaJaNai_2048p_V1_ESRGAN_70k.safetensors",
        4,
        2048,
        B4,
        "6bfee445b00d823de29425fc08ef946ec503e8c9cc7dcc6c588db8bcfa8a9122"
    ),
    ModelInfo {
        id: "illustrationjanai-v1-2x",
        title: "IllustrationJaNai 2x (colour)",
        file: "2x_IllustrationJaNai_V1_ESRGAN_120k.safetensors",
        arch: Arch::Esrgan,
        scale: 2,
        content: Content::Color,
        height: None,
        bytes: B2,
        sha256: "b824f999f1339848dfae86c7749af278007afdab4221546e06b9b8fa498e740a",
        license: LICENSE,
        author: AUTHOR,
        source: MJ_SRC,
        lite: false,
    },
    ModelInfo {
        id: "illustrationjanai-v1-4x",
        title: "IllustrationJaNai 4x (colour)",
        file: "4x_IllustrationJaNai_V1_ESRGAN_135k.safetensors",
        arch: Arch::Esrgan,
        scale: 4,
        content: Content::Color,
        height: None,
        bytes: B4,
        sha256: "a4f1925603df5d8151b80d31e5ab5adcf5a37fa91dd2c7450862797fa6dc2b4e",
        license: LICENSE,
        author: AUTHOR,
        source: MJ_SRC,
        lite: false,
    },
    ModelInfo {
        id: LAMA_ID,
        title: "Manga LaMa",
        file: "manga-lama-f16.safetensors",
        arch: Arch::Lama,
        scale: 1,
        content: Content::Inpaint,
        height: None,
        bytes: 101_904_726,
        sha256: "969b3abd517fadeb7ae92164f0bf364766924b6550da0e9a509fd9742d449733",
        license: "MIT",
        author: "dreMaz (AnimeMangaInpainting)",
        source: "https://huggingface.co/dreMaz/AnimeMangaInpainting",
        lite: false,
    },
    compact!(COMPACT_4X_ID, "Real-ESRGAN Compact 4x", 4),
    compact!(COMPACT_2X_ID, "Real-ESRGAN Compact 2x", 2),
    ModelInfo {
        id: MIGAN_ID,
        title: "MI-GAN",
        file: "migan-512-places2-f16.safetensors",
        arch: Arch::Migan,
        scale: 1,
        content: Content::Inpaint,
        height: None,
        bytes: 13_198_644,
        sha256: "76a73c53825e975c3aca4fcb1cf81521a92ca068117ff3a05ae54e7c109b8c68",
        license: "MIT",
        author: "Picsart AI Research (MI-GAN)",
        source: "https://github.com/Picsart-AI-Research/MI-GAN",
        lite: true,
    },
    genfill_file!(
        "genfill-text",
        "Generative Fill: text encoder",
        "sd15-text-ds8-f16.safetensors",
        246_669_099,
        "20776b79c17e88dfa2384ef314954053fc6a29695aaee5e8f3197ade701c4b83",
        OPENRAIL,
        DS8_AUTHOR,
        DS8_SRC
    ),
    genfill_file!(
        "genfill-vae-enc",
        "Generative Fill: image encoder",
        "sd15-vae-enc-ds8-f16.safetensors",
        68_339_472,
        "5390a6166e2b6edaa86f4ad6953aea49de738cfb2a4546234104ceeb65fbc296",
        OPENRAIL,
        DS8_AUTHOR,
        DS8_SRC
    ),
    genfill_file!(
        "genfill-vae-dec",
        "Generative Fill: image decoder",
        "sd15-vae-dec-ds8-f16.safetensors",
        98_996_014,
        "0a603b02b954887eea1c470809650f65d4fb59af0808abf174770e6ad82dcda3",
        OPENRAIL,
        DS8_AUTHOR,
        DS8_SRC
    ),
    genfill_file!(
        "genfill-taesd",
        "Generative Fill: preview decoder (TAESD)",
        "taesd-f16.safetensors",
        4_902_382,
        "1b0880ab864b309eba0a5fb5ca4c7bec74f87fd20122e310995cfd46da6efdf6",
        "MIT",
        "madebyollin (TAESD)",
        "https://github.com/madebyollin/taesd"
    ),
    genfill_file!(
        "genfill-unet-1",
        "Generative Fill: model, part 1 of 4",
        "ds8-inpaint-unet-f16.part0of4.safetensors",
        437_056_840,
        "29db50b5252ce1cf4e1df2b6ad295bd03f0f91a070905ffbf2d93ac3ad5c9543",
        OPENRAIL,
        DS8_AUTHOR,
        DS8_SRC
    ),
    genfill_file!(
        "genfill-unet-2",
        "Generative Fill: model, part 2 of 4",
        "ds8-inpaint-unet-f16.part1of4.safetensors",
        447_271_648,
        "29f20ed6691dc31d85dfe8841318df04ee4d3e173a978b50bc9a4259a196b1f6",
        OPENRAIL,
        DS8_AUTHOR,
        DS8_SRC
    ),
    genfill_file!(
        "genfill-unet-3",
        "Generative Fill: model, part 3 of 4",
        "ds8-inpaint-unet-f16.part2of4.safetensors",
        444_583_672,
        "f8647c228b0af5335fe71d6b8a7b017020c8a0f4264caead44045956bc596f86",
        OPENRAIL,
        DS8_AUTHOR,
        DS8_SRC
    ),
    genfill_file!(
        "genfill-unet-4",
        "Generative Fill: model, part 4 of 4",
        "ds8-inpaint-unet-f16.part3of4.safetensors",
        390_242_232,
        "7e5c0d02e5c848a4f530dddae9b5a71747c05b02a487c97824f0da64b7c86a5e",
        OPENRAIL,
        DS8_AUTHOR,
        DS8_SRC
    ),
    genfill_file!(
        "genfill-text-q4",
        "Generative Fill (light): text encoder, 4-bit",
        "sd15-text-ds8-q4.safetensors",
        89_018_851,
        "bd15fb3365e1af74b91187da1c89709428f2df9d24c760807920f063b952c233",
        OPENRAIL,
        DS8_AUTHOR,
        DS8_SRC
    ),
    genfill_file!(
        "genfill-ultra-unet",
        "Generative Fill (light): SDXS model, 4-bit",
        "sdxs-ds-unet-q4.safetensors",
        183_409_040,
        "2aa0f05df7d1ae0adf54af3f116e540668a118863a497122be887b9b779bb1ed",
        "CreativeML-OpenRAIL++-M",
        "IDKiro (SDXS-512-DreamShaper)",
        "https://huggingface.co/IDKiro/sdxs-512-dreamshaper"
    ),
    genfill_file!(
        GENFILL_FAST_LORA_ID,
        "Generative Fill: few-step sampler (TCD)",
        "tcd-sd15-lora-f16.safetensors",
        134_576_208,
        "877f74eb282991d8a05dc9ab191c2f9a2ea081a65b0455fa4cf791e68dbe66fd",
        "MIT",
        "Jianbin Zheng et al. (TCD)",
        "https://huggingface.co/h1t/TCD-SD15-LoRA"
    ),
];


/// The lite upscaler's ids.
pub const COMPACT_4X_ID: &str = "realesr-compact-4x";
pub const COMPACT_2X_ID: &str = "realesr-compact-2x";
/// The id of the MI-GAN (lite) inpainting model.
pub const MIGAN_ID: &str = "migan-512";

/// The few-step LoRA of Generative Fill's Standard tier.
pub const GENFILL_FAST_LORA_ID: &str = "genfill-tcd";

/// The files of Generative Fill's Standard tier, in load order (smallest first, the UNet last).
pub const GENFILL_STANDARD: &[&str] = &[
    "genfill-text",
    "genfill-taesd",
    "genfill-vae-enc",
    "genfill-vae-dec",
    "genfill-unet-1",
    "genfill-unet-2",
    "genfill-unet-3",
    "genfill-unet-4",
    GENFILL_FAST_LORA_ID,
];

/// The files of Generative Fill's Ultra-light tier (CPUs, phones): 277 MB.
pub const GENFILL_ULTRA: &[&str] = &["genfill-text-q4", "genfill-taesd", "genfill-ultra-unet"];

/// The id of the Manga LaMa inpainting model.
pub const LAMA_ID: &str = "manga-lama";

/// The model with this id.
pub fn model(id: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_files_are_unique_and_hashes_well_formed() {
        for (i, a) in MODELS.iter().enumerate() {
            assert_eq!(a.sha256.len(), 64);
            assert!(a.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            if a.arch == Arch::Esrgan {
                assert!(a.file.starts_with(&format!("{}x_", a.scale)));
            }
            for b in &MODELS[i + 1..] {
                assert_ne!(a.id, b.id);
                // Entries may share a file (the lite upscaler's two scales), then all of it.
                if a.file == b.file {
                    assert_eq!((a.sha256, a.bytes, a.arch), (b.sha256, b.bytes, b.arch));
                }
            }
        }
        assert_eq!(model("mangajanai-v1-4x-1200p").map(|m| m.scale), Some(4));
        assert!(model("nope").is_none());
        assert_eq!(MODELS[0].url("https://x/models/"), "https://x/models/2x_MangaJaNai_1200p_V1_ESRGAN_70k.safetensors");
    }
}
