//! What the AI Model Manager shows: one card per model or Generative Fill tier, grouped by what it
//! does. A card owns one or more catalog files (MangaJaNai's 14 heights, Generative Fill's parts);
//! files shared by several cards (TAESD) are downloaded once and kept while any card that uses it
//! is installed. The engine's `ai.models.*` commands take a card id where they take a model id.

use crate::catalog::{self, ModelInfo};
use crate::models::Status;

/// A card's section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Section {
    /// Erase text and objects (Clean with AI, the Remove tool).
    Clean,
    /// Enlarge pages (Upscale with AI).
    Upscale,
    /// Draw new content from a prompt (Generative Fill).
    Generative,
}

/// One card.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub id: &'static str,
    pub section: Section,
    /// Short English name (the UI translates it).
    pub title: &'static str,
    /// One English sentence (the UI translates it).
    pub description: &'static str,
    /// Catalog ids of its files.
    pub files: &'static [&'static str],
    /// 1..=5.
    pub quality: u8,
    /// 1..=5 (5: fastest).
    pub speed: u8,
    /// Needs a GPU to be usable (else it also runs on the CPU).
    pub gpu: bool,
    /// Weight precision, as shown ("f16", "8-bit", "4-bit").
    pub precision: &'static str,
    /// Memory it needs while running, MB (GPU memory when `gpu`).
    pub memory_mb: u32,
}

const MJ: &[&str] = &[
    "mangajanai-v1-2x-1200p",
    "mangajanai-v1-2x-1300p",
    "mangajanai-v1-2x-1400p",
    "mangajanai-v1-2x-1500p",
    "mangajanai-v1-2x-1600p",
    "mangajanai-v1-2x-1920p",
    "mangajanai-v1-2x-2048p",
    "mangajanai-v1-4x-1200p",
    "mangajanai-v1-4x-1300p",
    "mangajanai-v1-4x-1400p",
    "mangajanai-v1-4x-1500p",
    "mangajanai-v1-4x-1600p",
    "mangajanai-v1-4x-1920p",
    "mangajanai-v1-4x-2048p",
];

/// Card ids of the Generative Fill tiers.
pub const GENFILL_ULTRA_CARD: &str = "genfill-ultra";
pub const GENFILL_FAST_CARD: &str = "genfill-fast";
pub const GENFILL_STANDARD_CARD: &str = "genfill-standard";

/// Every card, in display order.
pub const CARDS: &[Card] = &[
    Card {
        id: "manga-lama",
        section: Section::Clean,
        title: "Manga LaMa",
        description: "Erases text and SFX and redraws screentone and lines.",
        files: &[catalog::LAMA_ID],
        quality: 5,
        speed: 3,
        gpu: true,
        precision: "f16",
        memory_mb: 900,
    },
    Card {
        id: "migan",
        section: Section::Clean,
        title: "MI-GAN (lite)",
        description: "A small cleaner for phones and computers without a GPU.",
        files: &[catalog::MIGAN_ID],
        quality: 3,
        speed: 5,
        gpu: false,
        precision: "f16",
        memory_mb: 300,
    },
    Card {
        id: "mangajanai",
        section: Section::Upscale,
        title: "MangaJaNai",
        description: "Sharp 2× and 4× upscaling of black-and-white manga pages.",
        files: MJ,
        quality: 5,
        speed: 2,
        gpu: true,
        precision: "f16",
        memory_mb: 1500,
    },
    Card {
        id: "illustrationjanai",
        section: Section::Upscale,
        title: "IllustrationJaNai",
        description: "2× and 4× upscaling of colour pages and covers.",
        files: &["illustrationjanai-v1-2x", "illustrationjanai-v1-4x"],
        quality: 5,
        speed: 2,
        gpu: true,
        precision: "f16",
        memory_mb: 1500,
    },
    Card {
        id: "realesr-compact",
        section: Section::Upscale,
        title: "Real-ESRGAN Compact (lite)",
        description: "Fast upscaling that runs well on any device.",
        files: &[catalog::COMPACT_4X_ID],
        quality: 3,
        speed: 5,
        gpu: false,
        precision: "f16",
        memory_mb: 200,
    },
    Card {
        id: GENFILL_ULTRA_CARD,
        section: Section::Generative,
        title: "Ultra-light",
        description: "Rough fills on any device, even without a GPU.",
        files: catalog::GENFILL_ULTRA,
        quality: 2,
        speed: 4,
        gpu: false,
        precision: "4-bit",
        memory_mb: 1000,
    },
    Card {
        id: GENFILL_FAST_CARD,
        section: Section::Generative,
        title: "Fast",
        description: "Good fills with a smaller download.",
        files: catalog::GENFILL_FAST,
        quality: 4,
        speed: 3,
        gpu: false,
        precision: "8-bit",
        memory_mb: 2500,
    },
    Card {
        id: GENFILL_STANDARD_CARD,
        section: Section::Generative,
        title: "Standard",
        description: "Detailed fills from a prompt, three variants in seconds on a GPU.",
        files: catalog::GENFILL_STANDARD,
        quality: 4,
        speed: 4,
        gpu: true,
        precision: "f16",
        memory_mb: 3500,
    },
];

/// The card with this id.
pub fn card(id: &str) -> Option<&'static Card> {
    CARDS.iter().find(|c| c.id == id)
}

impl Card {
    /// Its catalog entries (one per distinct file).
    pub fn models(&self) -> Vec<&'static ModelInfo> {
        let mut out: Vec<&'static ModelInfo> = Vec::new();
        for m in self.files.iter().filter_map(|id| catalog::model(id)) {
            if !out.iter().any(|o| o.file == m.file) {
                out.push(m);
            }
        }
        out
    }

    /// Download size of all its files.
    pub fn bytes(&self) -> u64 {
        self.models().iter().map(|m| m.bytes).sum()
    }
}

/// A card's overall state, from its files' statuses.
#[derive(Clone, Debug, PartialEq)]
pub enum CardState {
    NotDownloaded,
    /// Some files downloading or queued: bytes done and total over all its files, speed.
    Downloading { done: u64, total: u64, bytes_per_sec: f64 },
    /// Stopped part-way (paused or interrupted): resumes where it stopped.
    Paused { done: u64, total: u64 },
    Ready,
    /// A file failed (and why).
    Error(String),
    Checking,
    Unavailable(String),
}

impl CardState {
    /// Folds the statuses of a card's files (`(bytes, status)` per file).
    pub fn of(files: &[(u64, Status)]) -> Self {
        let total: u64 = files.iter().map(|(b, _)| *b).sum();
        let done: u64 = files
            .iter()
            .map(|(b, s)| match s {
                Status::Downloaded | Status::UpdateAvailable => *b,
                Status::Downloading { done, .. } | Status::Paused { done, .. } => *done,
                _ => 0,
            })
            .sum();
        if let Some(why) = files.iter().find_map(|(_, s)| if let Status::Unavailable(w) = s { Some(w.clone()) } else { None }) {
            return Self::Unavailable(why);
        }
        if files.iter().all(|(_, s)| s.is_downloaded()) {
            return Self::Ready;
        }
        if files.iter().any(|(_, s)| s.is_pending()) {
            let speed = files.iter().map(|(_, s)| if let Status::Downloading { bytes_per_sec, .. } = s { *bytes_per_sec } else { 0.0 }).sum();
            return Self::Downloading { done, total, bytes_per_sec: speed };
        }
        if let Some(why) = files.iter().find_map(|(_, s)| if let Status::Failed(w) = s { Some(w.clone()) } else { None }) {
            return Self::Error(why);
        }
        if files.iter().any(|(_, s)| matches!(s, Status::Checking)) {
            return Self::Checking;
        }
        if files.iter().any(|(_, s)| matches!(s, Status::Paused { .. })) {
            return Self::Paused { done, total };
        }
        Self::NotDownloaded
    }

    /// A short stable id for automation.
    pub fn id(&self) -> &'static str {
        match self {
            Self::NotDownloaded => "notDownloaded",
            Self::Downloading { .. } => "downloading",
            Self::Paused { .. } => "paused",
            Self::Ready => "ready",
            Self::Error(_) => "error",
            Self::Checking => "checking",
            Self::Unavailable(_) => "unavailable",
        }
    }
}

/// The files a card may delete: its own, except those another installed card still uses.
pub fn deletable(card: &Card, installed: &dyn Fn(&Card) -> bool) -> Vec<&'static ModelInfo> {
    let keep: Vec<&'static str> = CARDS.iter().filter(|c| c.id != card.id && installed(c)).flat_map(|c| c.models()).map(|m| m.file).collect();
    card.models().into_iter().filter(|m| !keep.contains(&m.file)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_card_file_is_in_the_catalog_and_every_file_has_a_card() {
        for c in CARDS {
            assert_eq!(c.models().len(), c.files.iter().filter_map(|id| catalog::model(id)).map(|m| m.file).collect::<std::collections::HashSet<_>>().len(), "{}", c.id);
            assert!((1..=5).contains(&c.quality) && (1..=5).contains(&c.speed));
        }
        let shown: Vec<&str> = CARDS.iter().flat_map(|c| c.models()).map(|m| m.file).collect();
        for m in crate::models::files() {
            assert!(shown.contains(&m.file), "{} has no card", m.id);
        }
    }

    #[test]
    fn shared_files_stay_while_another_card_uses_them() {
        let std_card = card(GENFILL_STANDARD_CARD).unwrap_or_else(|| panic!("card"));
        let ultra = card(GENFILL_ULTRA_CARD).unwrap_or_else(|| panic!("card"));
        let taesd = |v: &[&ModelInfo]| v.iter().any(|m| m.id == "genfill-taesd");
        assert!(taesd(&deletable(ultra, &|_| false)));
        assert!(!taesd(&deletable(ultra, &|c| c.id == std_card.id)));
    }

    #[test]
    fn states_fold() {
        let s = |v: Vec<Status>| CardState::of(&v.into_iter().map(|s| (100, s)).collect::<Vec<_>>());
        assert_eq!(s(vec![Status::Downloaded, Status::Downloaded]), CardState::Ready);
        assert_eq!(s(vec![Status::Downloaded, Status::Queued]), CardState::Downloading { done: 100, total: 200, bytes_per_sec: 0.0 });
        assert_eq!(s(vec![Status::Downloaded, Status::Paused { done: 50, total: 100 }]), CardState::Paused { done: 150, total: 200 });
        assert_eq!(s(vec![Status::Failed("x".into()), Status::NotDownloaded]), CardState::Error("x".into()));
        assert_eq!(s(vec![Status::NotDownloaded]), CardState::NotDownloaded);
    }
}
