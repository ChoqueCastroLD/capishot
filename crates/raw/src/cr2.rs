//! Canon CR2.
//!
//! CR2 is a TIFF file whose header carries `CR`, the major and minor version
//! and the offset of the raw IFD (IFD3) at bytes 8–15. The raw IFD stores one
//! lossless JPEG (T.81 process 14) whose decoded sample sequence fills the
//! sensor image in vertical slices: the private tag 0xC640 gives the slice
//! count, slice width and last slice width; slice 0 is filled row by row,
//! then slice 1, and so on. sRAW/mRAW (subsampled YCbCr) is not supported.
//!
//! Sensor geometry and the as-shot white balance come from the Canon maker
//! note (an IFD at the MakerNote offset, values relative to the TIFF header):
//! tag 0x00E0 (SensorInfo: sensor size and the image borders) and tag
//! 0x4001 (ColorData, whose as-shot RGGB levels sit at a version-dependent
//! position; candidates are validated before use). The black level is
//! measured on the masked border; the white level is the clipping point
//! found in the data.

use crate::error::{RawError, Result};
use crate::sensor::{BlackLevels, Cfa, Rect, Sensor};
use crate::tiff::{Ifd, Tiff, tag};
use crate::{Limits, RawFormat, ljpeg};

const CANON_SENSOR_INFO: u16 = 0x00E0;
const CANON_COLOR_DATA: u16 = 0x4001;

/// The Canon maker note IFD, if present.
pub(crate) fn maker_note(t: &Tiff, ifds: &[Ifd]) -> Option<Ifd> {
    let e = ifds.iter().find_map(|i| i.get(tag::MAKER_NOTE).copied())?;
    t.ifd_at(e.at, 0)
}

/// As-shot white-balance multipliers (R, G, B) from ColorData.
fn as_shot_wb(t: &Tiff, mn: &Ifd) -> Option<[f64; 3]> {
    let e = mn.get(CANON_COLOR_DATA)?;
    let v = t.uints(e);
    // Word offsets of WB_RGGBLevelsAsShot used by the ColorData versions.
    for off in [0x3F, 0x47, 0x19, 0x22] {
        let Some(l) = v.get(off..off + 4) else { continue };
        let [r, g1, g2, b] = [l[0], l[1], l[2], l[3]].map(f64::from);
        let g = (g1 + g2) / 2.0;
        let plausible = (256.0..16384.0).contains(&g) && (g1 - g2).abs() <= g * 0.05 && (0.25..8.0).contains(&(r / g)) && (0.25..8.0).contains(&(b / g));
        if plausible {
            return Some([r / g, 1.0, b / g]);
        }
    }
    None
}

/// The white (clipping) level: the largest value when a meaningful share of
/// samples piles up at it, else the full range of the declared precision.
pub(crate) fn clip_level(data: &[u16], precision: u32) -> f32 {
    let full = ((1u32 << precision.min(16)) - 1) as f32;
    let step = (data.len() / 2_000_000).max(1);
    let mut max = 0u16;
    for v in data.iter().step_by(step) {
        max = max.max(*v);
    }
    if max == 0 {
        return full;
    }
    let near = data.iter().step_by(step).filter(|&&v| u32::from(v) + u32::from(max / 256) >= u32::from(max)).count();
    let sampled = data.len().div_ceil(step);
    if near * 2000 >= sampled {
        // At least 0.05 % of samples at the top: that is the clipping point.
        // Back off slightly so the clipped plateau maps to white.
        f32::from(max) * 0.995
    } else {
        full
    }
}

/// Mean of a region per 2×2 position.
pub(crate) fn masked_black(data: &[u16], width: usize, area: Rect) -> Option<[f32; 4]> {
    if area.is_empty() || area.width < 2 || area.height < 2 {
        return None;
    }
    let mut sum = [0u64; 4];
    let mut n = [0u64; 4];
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            let v = *data.get(y * width + x)?;
            let k = (y & 1) * 2 + (x & 1);
            sum[k] += u64::from(v);
            n[k] += 1;
        }
    }
    if n.contains(&0) {
        return None;
    }
    Some([0, 1, 2, 3].map(|k| sum[k] as f32 / n[k] as f32))
}

pub(crate) fn decode(t: &Tiff, limits: &Limits) -> Result<Sensor> {
    let ifds = t.all_ifds();
    let ifd0 = t.ifd_at(t.first_ifd, 0).ok_or_else(|| RawError::malformed("CR2 has no IFD0"))?;
    let raw_off = t.u32_at(12).unwrap_or(0) as usize;
    let raw = t.ifd_at(raw_off, 0).ok_or_else(|| RawError::malformed("CR2 raw IFD is missing"))?;
    let off = t.tag_uint(&raw, tag::STRIP_OFFSETS).ok_or_else(|| RawError::malformed("CR2 raw data offset missing"))? as usize;
    let len = t.tag_uint(&raw, tag::STRIP_BYTE_COUNTS).map(|l| l as usize).unwrap_or(t.data.len().saturating_sub(off));
    let src = t.bytes(off, len).or_else(|| t.data.get(off..)).ok_or_else(|| RawError::malformed("CR2 raw data lies outside the file"))?;

    let frame = ljpeg::frame(src).map_err(|e| match e {
        RawError::Unsupported(m) if m.contains("subsampled") => RawError::unsupported("Canon sRAW / mRAW"),
        e => e,
    })?;
    let total = frame.samples().ok_or_else(|| RawError::malformed("CR2 frame too large"))?;
    let slices = t.tag_uints(&raw, tag::CR2_SLICE);
    let (width, widths) = match slices.as_slice() {
        [n, w, last] if *n < 64 && *w > 0 && *last > 0 => {
            let (n, w, last) = (*n as usize, *w as usize, *last as usize);
            let mut ws = vec![w; n];
            ws.push(last);
            (n * w + last, ws)
        }
        _ => {
            let w = frame.width * frame.components;
            (w, vec![w])
        }
    };
    if width == 0 || total % width != 0 {
        return Err(RawError::malformed("CR2 slices do not match the JPEG frame"));
    }
    let height = total / width;
    limits.check(width as u64, height as u64, 2)?;
    let (_, samples) = ljpeg::decode(src, total)?;

    // De-slice.
    let mut data = vec![0u16; width * height];
    let mut pos = 0usize;
    let mut x0 = 0usize;
    for sw in widths {
        for y in 0..height {
            let src = samples.get(pos..pos + sw).ok_or_else(|| RawError::malformed("CR2 slice data is short"))?;
            data[y * width + x0..y * width + x0 + sw].copy_from_slice(src);
            pos += sw;
        }
        x0 += sw;
    }

    let mut warnings = Vec::new();
    let mn = maker_note(t, &ifds);
    let full = Rect::new(0, 0, width, height);
    // SensorInfo: [1] width, [2] height, [5] left, [6] top, [7] right, [8] bottom border (inclusive).
    let info = mn.as_ref().map(|m| t.tag_uints(m, CANON_SENSOR_INFO)).unwrap_or_default();
    let active = match info.get(5..9) {
        Some(&[l, tp, r, b]) if r > l && b > tp => Rect::new(l as usize, tp as usize, (r - l + 1) as usize, (b - tp + 1) as usize).intersect(&full),
        _ => full,
    };
    let active = if active.is_empty() { full } else { active };
    // Black: the masked columns left of the image (skipping a few edge columns).
    let black = if active.x >= 16 {
        // Only the outer half: the columns next to the image area can be partly exposed.
        let area = Rect::new(4, active.y, active.x / 2 - 4, active.height);
        masked_black(&data, width, area)
    } else {
        None
    };
    let black = match black {
        Some(b) => BlackLevels { rows: 2, cols: 2, values: b.to_vec(), delta_h: Vec::new(), delta_v: Vec::new() },
        None => {
            warnings.push("no masked sensor area; black level assumed to be 0".to_string());
            BlackLevels::uniform(0.0)
        }
    };
    let white = clip_level(&data, u32::from(frame.precision));
    // Canon sensors are RGGB from the top-left of the image area.
    let cfa = Cfa { width: 2, height: 2, colors: vec![0, 1, 1, 2], origin_x: active.x, origin_y: active.y };
    let camera_wb = mn.as_ref().and_then(|m| as_shot_wb(t, m));
    // The black-level values index from the active area origin, but the masked
    // measurement used data parity: realign them to active-area parity.
    let black = realign_black(black, active);
    Ok(Sensor {
        format: RawFormat::Cr2,
        make: t.tag_ascii(&ifd0, tag::MAKE),
        model: t.tag_ascii(&ifd0, tag::MODEL),
        width,
        height,
        samples: 1,
        data,
        cfa: Some(cfa),
        linearization: None,
        black,
        white: [white; 3],
        active,
        crop: active,
        color: Default::default(),
        camera_wb,
        orientation: t.tag_uint(&ifd0, tag::ORIENTATION).map(|o| o as u16).filter(|o| (1..=8).contains(o)).unwrap_or(1),
        baseline_exposure: 0.0,
        gain_maps: Vec::new(),
        warnings,
    })
}

/// Black levels measured per data-parity 2×2 position, re-indexed from the active origin.
pub(crate) fn realign_black(b: BlackLevels, active: Rect) -> BlackLevels {
    if b.rows != 2 || b.cols != 2 || b.values.len() != 4 {
        return b;
    }
    let v = &b.values;
    let values = (0..4)
        .map(|k| {
            let (ax, ay) = (k & 1, k >> 1);
            let (x, y) = ((active.x + ax) & 1, (active.y + ay) & 1);
            v[y * 2 + x]
        })
        .collect();
    BlackLevels { values, ..b }
}
