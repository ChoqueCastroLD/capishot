//! The device profile and the automatic choice of model and device: full models (LaMa,
//! MangaJaNai) on a real GPU, lite ones (MI-GAN, Real-ESRGAN Compact) on the CPU, phones and
//! weak machines. The choice always carries its [`Reason`], so the UI can say why ("No GPU:
//! the lite model, 13 MB"); quality is never lowered silently.
//!
//! The shell fills a [`DeviceProfile`] (the browser: WebGPU adapter info,
//! `navigator.deviceMemory`, `hardwareConcurrency`, a mobile user agent; natively: the wgpu
//! adapter and the core count) and [`choose`] turns it plus the user's
//! [`Quality`] and [`DevicePref`] into a [`Choice`].

use serde::Serialize;

/// What kind of GPU the machine offers for AI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum GpuClass {
    /// Not known yet (still probing, or the shell can't tell).
    #[default]
    Unknown,
    /// No GPU (no WebGPU, no adapter).
    None,
    /// A software rasterizer (SwiftShader, lavapipe, llvmpipe, WARP): slower than the CPU path.
    Software,
    /// Integrated or mobile GPU.
    Integrated,
    /// Discrete GPU.
    Discrete,
}

impl GpuClass {
    /// Worth running models on.
    pub fn usable(self) -> bool {
        matches!(self, Self::Integrated | Self::Discrete)
    }

    /// Classifies a WebGPU adapter from `GPUAdapterInfo` (`isFallbackAdapter`, `vendor`,
    /// `architecture`, `description`) or a wgpu adapter's name.
    pub fn from_adapter(fallback: bool, vendor: &str, architecture: &str, description: &str) -> Self {
        let all = format!("{vendor} {architecture} {description}").to_ascii_lowercase();
        if fallback || ["swiftshader", "llvmpipe", "lavapipe", "softpipe", "microsoft basic render", "warp"].iter().any(|s| all.contains(s)) {
            return Self::Software;
        }
        if all.trim().is_empty() {
            // WebGPU hides details unless asked; an adapter that is not a fallback is hardware.
            return Self::Integrated;
        }
        // Known discrete names; everything else (Intel, Apple, AMD APUs, phones) is integrated.
        // Both are usable: the class only shows in the UI.
        let discrete = ["nvidia", "geforce", "rtx", "quadro", "radeon rx", "radeon pro", "arc a", "arc b"].iter().any(|s| all.contains(s));
        if discrete { Self::Discrete } else { Self::Integrated }
    }
}

/// What the shell knows about the machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProfile {
    pub gpu: GpuClass,
    /// The adapter's name, for the UI.
    pub gpu_name: String,
    /// Device memory in GB (`navigator.deviceMemory`: 0.25…8, capped by browsers), if known.
    pub memory_gb: Option<f32>,
    /// Logical cores, if known.
    pub cores: Option<usize>,
    /// A phone or tablet.
    pub mobile: bool,
    /// The CPU path runs on several threads (natively, or a cross-origin isolated page).
    pub cpu_threads: bool,
}

impl DeviceProfile {
    /// A phone, little memory or few cores: full models would be slow or run out of memory.
    pub fn weak(&self) -> bool {
        self.mobile || self.memory_gb.is_some_and(|m| m < 4.0) || self.cores.is_some_and(|c| c < 4)
    }

    /// Very little memory (≤ 2 GB): smaller tiles too.
    pub fn low_memory(&self) -> bool {
        self.memory_gb.is_some_and(|m| m <= 2.0)
    }
}

/// The user's quality choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Quality {
    /// Full models on a capable GPU, lite ones otherwise.
    #[default]
    Auto,
    /// Always the lite model.
    Fast,
    /// Always the full model.
    Best,
}

impl Quality {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" | "" => Some(Self::Auto),
            "fast" | "lite" => Some(Self::Fast),
            "best" | "full" => Some(Self::Best),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fast => "fast",
            Self::Best => "best",
        }
    }
}

/// The user's device choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DevicePref {
    #[default]
    Auto,
    Gpu,
    Cpu,
}

impl DevicePref {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" | "" => Some(Self::Auto),
            "gpu" => Some(Self::Gpu),
            "cpu" => Some(Self::Cpu),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }
}

/// Why [`choose`] picked what it picked (the UI turns it into one line).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Reason {
    /// A capable GPU: the full model.
    Gpu,
    /// The GPU isn't known yet: the full model, falling back to the CPU if there is none.
    GpuUnknown,
    /// No GPU: the lite model on the CPU.
    NoGpu,
    /// Only a software GPU: the lite model on the CPU.
    SoftwareGpu,
    /// A phone or tablet: the lite model.
    Mobile,
    /// Little memory or few cores: the lite model.
    Weak,
    /// The user chose Fast.
    UserFast,
    /// The user chose Best.
    UserBest,
    /// The user chose the CPU: the lite model (Quality Auto).
    UserCpu,
}

impl Reason {
    pub fn id(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::GpuUnknown => "gpuUnknown",
            Self::NoGpu => "noGpu",
            Self::SoftwareGpu => "softwareGpu",
            Self::Mobile => "mobile",
            Self::Weak => "weak",
            Self::UserFast => "userFast",
            Self::UserBest => "userBest",
            Self::UserCpu => "userCpu",
        }
    }
}

/// The resolved choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    /// Run the lite model.
    pub lite: bool,
    /// Run on the CPU (else try the GPU, falling back to the CPU when it fails).
    pub cpu: bool,
    pub reason: Reason,
    /// Halve the default tile sizes (very little memory).
    pub small_tiles: bool,
}

/// Picks the model tier and the device (see the module docs).
pub fn choose(p: &DeviceProfile, quality: Quality, device: DevicePref) -> Choice {
    let cpu = match device {
        DevicePref::Cpu => true,
        DevicePref::Gpu => false,
        DevicePref::Auto => matches!(p.gpu, GpuClass::None | GpuClass::Software),
    };
    let (lite, reason) = match quality {
        Quality::Fast => (true, Reason::UserFast),
        Quality::Best => (false, Reason::UserBest),
        Quality::Auto => {
            if device == DevicePref::Cpu {
                (true, Reason::UserCpu)
            } else if p.gpu == GpuClass::Software {
                (true, Reason::SoftwareGpu)
            } else if cpu || p.gpu == GpuClass::None {
                (true, Reason::NoGpu)
            } else if p.mobile {
                (true, Reason::Mobile)
            } else if p.weak() {
                (true, Reason::Weak)
            } else if p.gpu == GpuClass::Unknown {
                (false, Reason::GpuUnknown)
            } else {
                (false, Reason::Gpu)
            }
        }
    };
    Choice { lite, cpu, reason, small_tiles: p.low_memory() }
}

/// Parses a `navigator.userAgent` (or `userAgentData.mobile`-less fallback) for phones and
/// tablets.
pub fn mobile_user_agent(ua: &str) -> bool {
    let ua = ua.to_ascii_lowercase();
    ["android", "iphone", "ipad", "ipod", "mobile", "silk/", "kindle"].iter().any(|s| ua.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop(gpu: GpuClass) -> DeviceProfile {
        DeviceProfile { gpu, memory_gb: Some(8.0), cores: Some(8), ..Default::default() }
    }

    #[test]
    fn gpu_gets_full_models_cpu_gets_lite() {
        let c = choose(&desktop(GpuClass::Discrete), Quality::Auto, DevicePref::Auto);
        assert_eq!((c.lite, c.cpu, c.reason), (false, false, Reason::Gpu));
        let c = choose(&desktop(GpuClass::Integrated), Quality::Auto, DevicePref::Auto);
        assert_eq!((c.lite, c.cpu), (false, false));
        let c = choose(&desktop(GpuClass::None), Quality::Auto, DevicePref::Auto);
        assert_eq!((c.lite, c.cpu, c.reason), (true, true, Reason::NoGpu));
        let c = choose(&desktop(GpuClass::Software), Quality::Auto, DevicePref::Auto);
        assert_eq!((c.lite, c.cpu, c.reason), (true, true, Reason::SoftwareGpu));
        // Not known yet: try the GPU with the full model (the worker falls back to the CPU).
        let c = choose(&DeviceProfile::default(), Quality::Auto, DevicePref::Auto);
        assert_eq!((c.lite, c.cpu, c.reason), (false, false, Reason::GpuUnknown));
    }

    #[test]
    fn phones_and_weak_machines_get_lite() {
        let phone = DeviceProfile { gpu: GpuClass::Integrated, mobile: true, memory_gb: Some(4.0), cores: Some(8), ..Default::default() };
        let c = choose(&phone, Quality::Auto, DevicePref::Auto);
        assert_eq!((c.lite, c.cpu, c.reason), (true, false, Reason::Mobile));
        let weak = DeviceProfile { gpu: GpuClass::Integrated, memory_gb: Some(2.0), cores: Some(4), ..Default::default() };
        let c = choose(&weak, Quality::Auto, DevicePref::Auto);
        assert_eq!((c.lite, c.reason, c.small_tiles), (true, Reason::Weak, true));
        let few = DeviceProfile { gpu: GpuClass::Discrete, cores: Some(2), ..Default::default() };
        assert!(choose(&few, Quality::Auto, DevicePref::Auto).lite);
    }

    #[test]
    fn user_choices_win() {
        let p = desktop(GpuClass::Discrete);
        let c = choose(&p, Quality::Fast, DevicePref::Auto);
        assert_eq!((c.lite, c.cpu, c.reason), (true, false, Reason::UserFast));
        let c = choose(&desktop(GpuClass::None), Quality::Best, DevicePref::Auto);
        assert_eq!((c.lite, c.cpu, c.reason), (false, true, Reason::UserBest));
        let c = choose(&p, Quality::Auto, DevicePref::Cpu);
        assert_eq!((c.lite, c.cpu, c.reason), (true, true, Reason::UserCpu));
        let c = choose(&p, Quality::Best, DevicePref::Cpu);
        assert_eq!((c.lite, c.cpu), (false, true));
        // GPU forced on a machine without one: still the GPU path (the runner falls back to
        // the CPU), and the lite model since there is no GPU to run the full one on.
        let c = choose(&desktop(GpuClass::None), Quality::Auto, DevicePref::Gpu);
        assert_eq!((c.lite, c.cpu, c.reason), (true, false, Reason::NoGpu));
    }

    #[test]
    fn adapters_and_user_agents() {
        assert_eq!(GpuClass::from_adapter(true, "", "", ""), GpuClass::Software);
        assert_eq!(GpuClass::from_adapter(false, "google", "swiftshader", ""), GpuClass::Software);
        assert_eq!(GpuClass::from_adapter(false, "nvidia", "ampere", ""), GpuClass::Discrete);
        assert_eq!(GpuClass::from_adapter(false, "intel", "gen-12lp", ""), GpuClass::Integrated);
        assert_eq!(GpuClass::from_adapter(false, "apple", "metal-3", ""), GpuClass::Integrated);
        assert_eq!(GpuClass::from_adapter(false, "", "", ""), GpuClass::Integrated);
        assert!(mobile_user_agent("Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 Mobile Safari/537.36"));
        assert!(mobile_user_agent("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X)"));
        assert!(!mobile_user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/141.0 Safari/537.36"));
    }
}
