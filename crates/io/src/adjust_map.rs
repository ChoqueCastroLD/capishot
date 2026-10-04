//! Mapping between PSD adjustment-layer blocks and [`Adjustment`].
//!
//! Binary layouts follow the Adobe spec ("Adjustment layer" section). The
//! mapped subset keeps the master/composite parameters; other channels of
//! Levels/Curves/Hue-Sat are written back with defaults.

use photocraft_doc::Adjustment;
use photocraft_doc::adjust::{CurvePoint, LevelsChannel};
use photocraft_psd::descriptor::{Descriptor, Value, VersionedDescriptor};

/// All PSD adjustment keys recognized as adjustment layers.
pub const ADJUSTMENT_KEYS: [&[u8; 4]; 16] =
    [b"levl", b"curv", b"hue2", b"brit", b"nvrt", b"thrs", b"post", b"expA", b"vibA", b"blnc", b"mixr", b"grdm", b"phfl", b"selc", b"blwh", b"clrL"];

fn be16(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}
fn bei16(d: &[u8], at: usize) -> Option<i16> {
    be16(d, at).map(|v| v as i16)
}
fn bef32(d: &[u8], at: usize) -> Option<f32> {
    d.get(at..at + 4).map(|b| f32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn unsupported(key: &[u8; 4], data: &[u8]) -> Adjustment {
    Adjustment::Unsupported { psd_key: String::from_utf8_lossy(key).into_owned(), raw: data.to_vec() }
}

fn levels_rec(d: &[u8], at: usize) -> Option<LevelsChannel> {
    Some(LevelsChannel {
        in_black: f32::from(be16(d, at)?) / 255.0,
        in_white: f32::from(be16(d, at + 2)?) / 255.0,
        out_black: f32::from(be16(d, at + 4)?) / 255.0,
        out_white: f32::from(be16(d, at + 6)?) / 255.0,
        gamma: f32::from(be16(d, at + 8)?) / 100.0,
    })
}

fn parse_curves(d: &[u8]) -> Option<Adjustment> {
    // pad(1) version(2) bitmap(4)
    let version = be16(d, 1)?;
    if version != 1 && version != 4 {
        return None;
    }
    let bits = u32::from_be_bytes(d.get(3..7)?.try_into().ok()?);
    let mut at = 7;
    let line = || vec![CurvePoint { input: 0.0, output: 0.0 }, CurvePoint { input: 1.0, output: 1.0 }];
    let mut curves: Vec<Vec<CurvePoint>> = vec![line(), line(), line(), line()];
    for bit in 0..32usize {
        if bits & (1 << bit) == 0 {
            continue;
        }
        let n = usize::from(be16(d, at)?);
        at += 2;
        let mut pts = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            let out = be16(d, at)?;
            let inp = be16(d, at + 2)?;
            at += 4;
            pts.push(CurvePoint { input: f32::from(inp) / 255.0, output: f32::from(out) / 255.0 });
        }
        if let Some(slot) = curves.get_mut(bit) {
            *slot = pts;
        }
    }
    let mut it = curves.into_iter();
    let master = it.next()?;
    let (r, g, b) = (it.next()?, it.next()?, it.next()?);
    Some(Adjustment::Curves { master, per_channel: [r, g, b] })
}

fn desc_num(d: &Descriptor, key: &str) -> Option<f32> {
    match d.get(key)? {
        Value::Integer(v) => Some(*v as f32),
        Value::Double(v) => Some(*v as f32),
        Value::UnitFloat { value, .. } => Some(*value as f32),
        _ => None,
    }
}

fn desc_bool(d: &Descriptor, key: &str) -> Option<bool> {
    match d.get(key)? {
        Value::Boolean(b) => Some(*b),
        _ => None,
    }
}

/// Channel interpretation of per-channel Levels/Curves records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channels {
    /// Records 1..=3 are R, G, B.
    Rgb,
    /// Record 1 is the gray channel (applied to all three display channels).
    Gray,
    /// Channel records do not map to display RGB (CMYK, Lab); ignored.
    Other,
}

/// Parses an adjustment block. `cged` is the optional `CgEd` block data
/// (modern brightness/contrast parameters). Levels/Curves store the
/// composite record first, then one per document channel (see [`Channels`]).
pub fn parse(key: &[u8; 4], data: &[u8], cged: Option<&[u8]>, channels: Channels) -> Adjustment {
    let mut a = parse_any(key, data, cged);
    match (&mut a, channels) {
        (_, Channels::Rgb) => {}
        (Adjustment::Levels { per_channel, .. }, Channels::Gray) => {
            let g = per_channel[0].clone();
            *per_channel = [g.clone(), g.clone(), g];
        }
        (Adjustment::Curves { per_channel, .. }, Channels::Gray) => {
            let g = per_channel[0].clone();
            *per_channel = [g.clone(), g.clone(), g];
        }
        (Adjustment::Levels { per_channel, .. }, Channels::Other) => *per_channel = Default::default(),
        (Adjustment::Curves { per_channel, .. }, Channels::Other) => {
            let line = || vec![CurvePoint { input: 0.0, output: 0.0 }, CurvePoint { input: 1.0, output: 1.0 }];
            *per_channel = [line(), line(), line()];
        }
        _ => {}
    }
    a
}

fn parse_any(key: &[u8; 4], data: &[u8], cged: Option<&[u8]>) -> Adjustment {
    let parsed = match key {
        b"nvrt" => Some(Adjustment::Invert),
        b"thrs" => be16(data, 0).map(|v| Adjustment::Threshold { level: f32::from(v) / 255.0 }),
        b"post" => be16(data, 0).map(|v| Adjustment::Posterize { levels: u32::from(v) }),
        b"brit" => {
            let modern = cged.and_then(|c| VersionedDescriptor::parse_prefix(c).ok()).and_then(|(v, _)| {
                let d = v.descriptor;
                Some(Adjustment::BrightnessContrast {
                    brightness: desc_num(&d, "Brgh")?,
                    contrast: desc_num(&d, "Cntr")?,
                    legacy: desc_bool(&d, "useLegacy").unwrap_or(false),
                })
            });
            modern
                .or_else(|| Some(Adjustment::BrightnessContrast { brightness: f32::from(bei16(data, 0)?), contrast: f32::from(bei16(data, 2)?), legacy: true }))
        }
        b"hue2" => (|| {
            let colorize = *data.get(2)? != 0;
            let base = if colorize { 4 } else { 10 };
            Some(Adjustment::HueSaturation {
                hue: f32::from(bei16(data, base)?),
                saturation: f32::from(bei16(data, base + 2)?),
                lightness: f32::from(bei16(data, base + 4)?),
                colorize,
            })
        })(),
        b"expA" => (|| Some(Adjustment::Exposure { exposure: bef32(data, 2)?, offset: bef32(data, 6)?, gamma: bef32(data, 10)? }))(),
        b"levl" => (|| {
            let m = levels_rec(data, 2)?;
            let r = levels_rec(data, 12)?;
            let g = levels_rec(data, 22)?;
            let b = levels_rec(data, 32)?;
            Some(Adjustment::Levels { master: m, per_channel: [r, g, b] })
        })(),
        b"curv" => parse_curves(data),
        b"selc" => parse_selective(data),
        b"clrL" => parse_lookup(data),
        b"grdm" => parse_gradient_map(data),
        _ => None,
    };
    parsed.unwrap_or_else(|| unsupported(key, data))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// `grdm` (Adobe spec, "Gradient settings"): version (1, or 3 with an interpolation method
/// key), reverse, dither, [method], name, colour stops (location /4096, midpoint %, colour
/// space + four u16 components, 2 bytes), transparency stops, then smoothness (/4096) among
/// noise-gradient fields. Smoothness, midpoints and the method are baked into dense stops like
/// gradient fills. Noise gradients (no colour stops) and non-RGB stops stay unsupported.
fn parse_gradient_map(d: &[u8]) -> Option<Adjustment> {
    let version = be16(d, 0)?;
    let reverse = *d.get(2)? != 0;
    let (method, mut at) = match version {
        1 => (None, 4),
        3 => (Some(d.get(4..8)?), 8),
        _ => return None,
    };
    at += 4 + be32(d, at)? as usize * 2;
    let n = usize::from(be16(d, at)?);
    at += 2;
    let mut stops = Vec::with_capacity(n);
    let mut mids = Vec::with_capacity(n);
    for _ in 0..n {
        let loc = (be32(d, at)? as f32 / 4096.0).clamp(0.0, 1.0);
        let mid = be32(d, at + 4)? as f32 / 100.0;
        if be16(d, at + 8)? != 0 {
            return None;
        }
        let c = |k: usize| be16(d, at + 10 + 2 * k).map(|v| f32::from(v) / 65535.0);
        stops.push((loc, photocraft_color::Color::rgb(c(0)?, c(1)?, c(2)?)));
        mids.push(mid);
        at += 20;
    }
    if stops.len() < 2 {
        return None;
    }
    let nt = usize::from(be16(d, at)?);
    at += 2 + nt * 10 + 2; // transparency stops, expansion count
    let smooth = f32::from(be16(d, at)?) / 4096.0;
    // Midpoint k applies to the segment after stop k (in location order).
    let mut order: Vec<usize> = (0..stops.len()).collect();
    order.sort_by(|a, b| stops[*a].0.total_cmp(&stops[*b].0));
    let mids: Vec<f32> = order.iter().skip(1).map(|i| mids[*i]).collect();
    let baked = crate::gradient_bake::bake(stops, &mids, smooth, crate::gradient_bake::Method::from_code(method));
    let mut stops: Vec<(f32, [f32; 3])> = baked.iter().map(|(t, c)| (*t, c.to_rgb())).collect();
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(Adjustment::GradientMap { stops, reverse })
}

/// `grdm` version 1 for [`Adjustment::GradientMap`] (Classic interpolation, Smoothness 0, so
/// the stops are reproduced exactly).
fn write_gradient_map(stops: &[(f32, [f32; 3])], reverse: bool) -> Vec<u8> {
    let mut v = Vec::new();
    put16(&mut v, 1);
    v.push(u8::from(reverse));
    v.push(0);
    let name: Vec<u16> = "Custom".encode_utf16().chain(std::iter::once(0)).collect();
    v.extend_from_slice(&(name.len() as u32).to_be_bytes());
    for u in name {
        put16(&mut v, u);
    }
    put16(&mut v, stops.len().min(usize::from(u16::MAX)) as u16);
    for (t, c) in stops.iter().take(usize::from(u16::MAX)) {
        v.extend_from_slice(&((t.clamp(0.0, 1.0) * 4096.0).round() as u32).to_be_bytes());
        v.extend_from_slice(&50u32.to_be_bytes());
        put16(&mut v, 0);
        for x in c {
            put16(&mut v, (x.clamp(0.0, 1.0) * 65535.0).round() as u16);
        }
        put16(&mut v, 0); // fourth component
        put16(&mut v, 0);
    }
    put16(&mut v, 2);
    for t in [0u32, 4096] {
        v.extend_from_slice(&t.to_be_bytes());
        v.extend_from_slice(&50u32.to_be_bytes());
        put16(&mut v, 255);
    }
    put16(&mut v, 2); // expansion count
    put16(&mut v, 0); // smoothness
    put16(&mut v, 32); // length
    put16(&mut v, 0); // mode
    v.extend_from_slice(&0u32.to_be_bytes()); // random seed
    put16(&mut v, 0); // showing transparency
    put16(&mut v, 0); // using vector colour
    v.extend_from_slice(&2048u32.to_be_bytes()); // roughness
    put16(&mut v, 3); // colour model
    for x in [0u16, 0, 0, 0, 0x8000, 0x8000, 0x8000, 0x8000] {
        put16(&mut v, x);
    }
    put16(&mut v, 0);
    v
}

/// `selc`: version, method (0 relative / 1 absolute), then 10 CMYK records of i16 percentages;
/// record 0 is reserved, records 1..=9 are reds … blacks.
fn parse_selective(d: &[u8]) -> Option<Adjustment> {
    if be16(d, 0)? != 1 || d.len() < 84 {
        return None;
    }
    let relative = be16(d, 2)? == 0;
    let mut adjustments = [[0.0f32; 4]; 9];
    for (r, rec) in adjustments.iter_mut().enumerate() {
        for (k, v) in rec.iter_mut().enumerate() {
            *v = f32::from(bei16(d, 4 + (r + 1) * 8 + k * 2)?);
        }
    }
    Some(Adjustment::SelectiveColor { relative, adjustments })
}

fn desc_text(d: &Descriptor, key: &str) -> Option<String> {
    match d.get(key)? {
        Value::Text(t) => Some(t.to_string_lossy()),
        _ => None,
    }
}

fn desc_enum<'a>(d: &'a Descriptor, key: &str) -> Option<&'a [u8]> {
    match d.get(key)? {
        Value::Enumerated { value, .. } => Some(value.as_bytes()),
        _ => None,
    }
}

/// `clrL`: version 1 + a versioned descriptor carrying the LUT file itself (`LUT3DFileData`, in
/// the format named by `LUTFormat`). Profile-based lookups (abstract / device link) have no
/// table and stay [`Adjustment::Unsupported`].
fn parse_lookup(d: &[u8]) -> Option<Adjustment> {
    if be16(d, 0)? != 1 {
        return None;
    }
    let (v, _) = VersionedDescriptor::parse_prefix(d.get(2..)?).ok()?;
    let desc = v.descriptor;
    let bytes = match desc.get("LUT3DFileData")? {
        Value::RawData(b) if !b.is_empty() => b,
        _ => return None,
    };
    let ext = match desc_enum(&desc, "LUTFormat") {
        Some(b"LUTFormat3DL") => "x.3dl",
        Some(b"LUTFormatLOOK") => "x.look",
        _ => "x.cube",
    };
    let lut = photocraft_cms::lutfile::parse(ext, bytes).ok()?;
    let name = desc_text(&desc, "LUT3DFileName").or_else(|| desc_text(&desc, "NM  ")).unwrap_or_default();
    Some(Adjustment::ColorLookup {
        name,
        size: lut.size as u32,
        lut: Some(std::sync::Arc::new(lut.data)),
        tetrahedral: false,
        dither: desc_bool(&desc, "Dthr").unwrap_or(false),
    })
}

fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn q255(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u16
}
fn clamp_i16(v: f32) -> i16 {
    v.round().clamp(-32768.0, 32767.0) as i16
}

fn levels_write(v: &mut Vec<u8>, c: &LevelsChannel) {
    put16(v, q255(c.in_black));
    put16(v, q255(c.in_white));
    put16(v, q255(c.out_black));
    put16(v, q255(c.out_white));
    put16(v, (c.gamma * 100.0).round().clamp(1.0, 999.0) as u16);
}

/// Serializes an adjustment into its PSD blocks: `(key, data)` pairs
/// (Brightness/Contrast also writes a `CgEd` descriptor).
pub fn write(adj: &Adjustment) -> Vec<([u8; 4], Vec<u8>)> {
    let mut v = Vec::new();
    match adj {
        Adjustment::Invert => return vec![(*b"nvrt", Vec::new())],
        Adjustment::Threshold { level } => {
            put16(&mut v, q255(*level).max(1));
            put16(&mut v, 0);
            return vec![(*b"thrs", v)];
        }
        Adjustment::Posterize { levels } => {
            put16(&mut v, (*levels).clamp(2, 255) as u16);
            put16(&mut v, 0);
            return vec![(*b"post", v)];
        }
        Adjustment::BrightnessContrast { brightness, contrast, legacy } => {
            v.extend_from_slice(&clamp_i16(*brightness).to_be_bytes());
            v.extend_from_slice(&clamp_i16(*contrast).to_be_bytes());
            put16(&mut v, 127);
            v.push(0);
            let d = Descriptor::new("null")
                .with("Vrsn", Value::Integer(1))
                .with("Brgh", Value::Integer(brightness.round() as i32))
                .with("Cntr", Value::Integer(contrast.round() as i32))
                .with("means", Value::Integer(127))
                .with("Lab ", Value::Boolean(false))
                .with("useLegacy", Value::Boolean(*legacy))
                .with("Auto", Value::Boolean(false));
            return vec![(*b"brit", v), (*b"CgEd", VersionedDescriptor::new(d).to_bytes())];
        }
        Adjustment::HueSaturation { hue, saturation, lightness, colorize } => {
            put16(&mut v, 2);
            v.push(u8::from(*colorize));
            v.push(0);
            for _ in 0..2 {
                for x in [hue, saturation, lightness] {
                    v.extend_from_slice(&clamp_i16(*x).to_be_bytes());
                }
            }
            // Six default hue ranges (reds, yellows, greens, cyans, blues, magentas).
            let ranges: [[u16; 4]; 6] =
                [[315, 345, 15, 45], [15, 45, 75, 105], [75, 105, 135, 165], [135, 165, 195, 225], [195, 225, 255, 285], [255, 285, 315, 345]];
            for r in ranges {
                for x in r {
                    put16(&mut v, x);
                }
                v.extend_from_slice(&[0; 6]);
            }
            return vec![(*b"hue2", v)];
        }
        Adjustment::Exposure { exposure, offset, gamma } => {
            put16(&mut v, 1);
            for x in [exposure, offset, gamma] {
                v.extend_from_slice(&x.to_be_bytes());
            }
            v.push(1); // color space flag (spec: "1 byte")
            return vec![(*b"expA", v)];
        }
        Adjustment::Levels { master, per_channel } => {
            put16(&mut v, 2);
            levels_write(&mut v, master);
            for c in per_channel {
                levels_write(&mut v, c);
            }
            for _ in 4..29 {
                levels_write(&mut v, &LevelsChannel::default());
            }
            return vec![(*b"levl", v)];
        }
        Adjustment::Curves { master, per_channel } => {
            v.push(0);
            put16(&mut v, 1);
            v.extend_from_slice(&0b1111u32.to_be_bytes());
            for c in std::iter::once(master).chain(per_channel.iter()) {
                put16(&mut v, c.len().min(19) as u16);
                for p in c.iter().take(19) {
                    put16(&mut v, q255(p.output));
                    put16(&mut v, q255(p.input));
                }
            }
            return vec![(*b"curv", v)];
        }
        Adjustment::SelectiveColor { relative, adjustments } => {
            put16(&mut v, 1);
            put16(&mut v, u16::from(!*relative));
            v.extend_from_slice(&[0; 8]);
            for rec in adjustments {
                for x in rec {
                    v.extend_from_slice(&clamp_i16(x.clamp(-100.0, 100.0)).to_be_bytes());
                }
            }
            return vec![(*b"selc", v)];
        }
        Adjustment::ColorLookup { name, lut: Some(table), size, dither, .. } => {
            let file = photocraft_cms::lutfile::LutFile { title: String::new(), size: *size as usize, data: table.to_vec() };
            let en =
                |t: &str, val: &str| Value::Enumerated { type_id: photocraft_psd::descriptor::Id::new(t), value: photocraft_psd::descriptor::Id::new(val) };
            let text = |t: &str| Value::Text(photocraft_psd::descriptor::UnicodeString::new_nul(t));
            let d = Descriptor::new("null")
                .with("lookupType", en("colorLookupType", "3DLUT"))
                .with("NM  ", text(name))
                .with("Dthr", Value::Boolean(*dither))
                .with("profile", Value::RawData(Vec::new()))
                .with("LUTFormat", en("LUTFormatType", "LUTFormatCUBE"))
                .with("dataOrder", en("colorLookupOrder", "rgbOrder"))
                .with("tableOrder", en("colorLookupOrder", "bgrOrder"))
                .with("LUT3DFileData", Value::RawData(photocraft_cms::lutfile::write_cube(&file).into_bytes()))
                .with("LUT3DFileName", text(name));
            put16(&mut v, 1);
            v.extend_from_slice(&VersionedDescriptor::new(d).to_bytes());
            return vec![(*b"clrL", v)];
        }
        Adjustment::GradientMap { stops, reverse } if stops.len() >= 2 => return vec![(*b"grdm", write_gradient_map(stops, *reverse))],
        Adjustment::Unsupported { psd_key, raw } => {
            let k = psd_key.as_bytes();
            if k.len() == 4 {
                return vec![([k[0], k[1], k[2], k[3]], raw.clone())];
            }
        }
        _ => {}
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_map_v3_methods_and_smoothness() {
        let classic = vec![(0.0, [0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 0.0])];
        let v1 = write_gradient_map(&classic, false);
        // Version 3 inserts the interpolation method after reverse/dither.
        let v3 = |m: &[u8; 4]| {
            let mut v = v1.clone();
            v[1] = 3;
            v.splice(4..4, m.iter().copied());
            v
        };
        let at_t = |data: &[u8], t: f32| match parse(b"grdm", data, None, Channels::Rgb) {
            Adjustment::GradientMap { stops, .. } => {
                let i = stops.windows(2).position(|w| w[0].0 <= t && t <= w[1].0).unwrap();
                let (x, y) = (stops[i], stops[i + 1]);
                let u = (t - x.0) / (y.0 - x.0).max(1e-6);
                std::array::from_fn::<f32, 3, _>(|k| x.1[k] + (y.1[k] - x.1[k]) * u)
            }
            other => panic!("{other:?}"),
        };
        let mid = |data: &[u8]| at_t(data, 0.5);
        let c = mid(&v3(b"Gcls"));
        assert!((c[0] - 0.5).abs() < 1e-3 && (c[2] - 0.5).abs() < 1e-3, "classic = sRGB lerp {c:?}");
        let p = mid(&v3(b"Perc"));
        assert!((p[0] - c[0]).abs() > 0.02, "perceptual differs from classic {p:?}");
        // Smoothness 100 % (4096) bends a three-stop ramp.
        let three = vec![(0.0, [0.0; 3]), (0.25, [1.0, 0.0, 0.0]), (0.75, [0.0, 0.0, 1.0]), (1.0, [1.0; 3])];
        let mut smooth = write_gradient_map(&three, false);
        let at = smooth.len() - 2 - 16 - 2 - 4 - 2 - 2 - 4 - 2 - 2 - 2;
        smooth[at..at + 2].copy_from_slice(&4096u16.to_be_bytes());
        let (a, b) = (at_t(&write_gradient_map(&three, false), 0.4), at_t(&smooth, 0.4));
        assert!((0..3).any(|k| (a[k] - b[k]).abs() > 1e-3), "{a:?} {b:?}");
        // Noise gradients (no colour stops) stay unsupported.
        assert!(matches!(parse(b"grdm", &[0, 1, 0, 0, 0, 0, 0, 0, 0, 0], None, Channels::Rgb), Adjustment::Unsupported { .. }));
    }

    fn rt(a: Adjustment) {
        let blocks = write(&a);
        assert!(!blocks.is_empty(), "{a:?}");
        let cged = blocks.iter().find(|b| &b.0 == b"CgEd").map(|b| &b.1[..]);
        let back = parse(&blocks[0].0, &blocks[0].1, cged, Channels::Rgb);
        assert_eq!(back, a);
    }

    #[test]
    fn roundtrips() {
        rt(Adjustment::Invert);
        rt(Adjustment::Threshold { level: 128.0 / 255.0 });
        rt(Adjustment::Posterize { levels: 4 });
        rt(Adjustment::BrightnessContrast { brightness: 20.0, contrast: -10.0, legacy: false });
        rt(Adjustment::BrightnessContrast { brightness: -150.0, contrast: 100.0, legacy: true });
        rt(Adjustment::HueSaturation { hue: 30.0, saturation: -20.0, lightness: 5.0, colorize: false });
        rt(Adjustment::HueSaturation { hue: 200.0, saturation: 50.0, lightness: 0.0, colorize: true });
        rt(Adjustment::Exposure { exposure: 1.5, offset: -0.01, gamma: 0.9 });
        let lc = |a: u16, b: u16| LevelsChannel { in_black: f32::from(a) / 255.0, in_white: f32::from(b) / 255.0, gamma: 1.2, out_black: 0.0, out_white: 1.0 };
        rt(Adjustment::Levels { master: lc(10, 240), per_channel: [lc(0, 255), lc(5, 250), lc(20, 200)] });
        let pts = |v: &[(u8, u8)]| v.iter().map(|&(i, o)| CurvePoint { input: f32::from(i) / 255.0, output: f32::from(o) / 255.0 }).collect::<Vec<_>>();
        rt(Adjustment::Curves {
            master: pts(&[(0, 0), (128, 150), (255, 255)]),
            per_channel: [pts(&[(0, 10), (255, 255)]), pts(&[(0, 0), (255, 245)]), pts(&[(0, 0), (64, 32), (255, 255)])],
        });
        rt(Adjustment::Unsupported { psd_key: "selc".into(), raw: vec![1, 2, 3] });
        rt(Adjustment::GradientMap { stops: vec![(0.0, [0.0, 0.0, 0.0]), (0.5, [1.0, 0.0, 0.0]), (1.0, [1.0, 1.0, 1.0])], reverse: true });
        rt(Adjustment::SelectiveColor { relative: true, adjustments: std::array::from_fn(|r| [r as f32 * 10.0 - 40.0, 5.0, -100.0, 100.0]) });
        rt(Adjustment::SelectiveColor { relative: false, adjustments: [[0.0; 4]; 9] });
        let id = photocraft_cms::lutfile::LutFile::identity(5);
        rt(Adjustment::ColorLookup { name: "Look.cube".into(), lut: Some(std::sync::Arc::new(id.data)), size: 5, tetrahedral: false, dither: true });
    }

    #[test]
    fn selective_color_synthetic_block() {
        // Absolute; reds = (+10, -20, +30, -40), blacks = (0, 0, 0, 25).
        let mut d = vec![0, 1, 0, 1];
        d.extend([0u8; 8]);
        for r in 0..9 {
            let rec: [i16; 4] = match r {
                0 => [10, -20, 30, -40],
                8 => [0, 0, 0, 25],
                _ => [0; 4],
            };
            for x in rec {
                d.extend(x.to_be_bytes());
            }
        }
        let a = parse(b"selc", &d, None, Channels::Rgb);
        let Adjustment::SelectiveColor { relative, adjustments } = &a else { panic!("{a:?}") };
        assert!(!relative);
        assert_eq!(adjustments[0], [10.0, -20.0, 30.0, -40.0]);
        assert_eq!(adjustments[8], [0.0, 0.0, 0.0, 25.0]);
        // Written back byte-exact.
        assert_eq!(write(&a)[0].1, d);
    }

    #[test]
    fn color_lookup_synthetic_block() {
        let cube = b"TITLE \"t\"\nLUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
        let en = |t: &str, val: &str| Value::Enumerated { type_id: photocraft_psd::descriptor::Id::new(t), value: photocraft_psd::descriptor::Id::new(val) };
        let desc = Descriptor::new("null")
            .with("lookupType", en("colorLookupType", "3DLUT"))
            .with("NM  ", Value::Text(photocraft_psd::descriptor::UnicodeString::new_nul("Id")))
            .with("Dthr", Value::Boolean(true))
            .with("LUTFormat", en("LUTFormatType", "LUTFormatCUBE"))
            .with("LUT3DFileData", Value::RawData(cube.to_vec()));
        let mut d = vec![0, 1];
        d.extend(VersionedDescriptor::new(desc).to_bytes());
        let a = parse(b"clrL", &d, None, Channels::Rgb);
        let Adjustment::ColorLookup { name, lut: Some(t), size: 2, dither: true, .. } = &a else { panic!("{a:?}") };
        assert_eq!(name, "Id");
        assert_eq!(&t[..6], &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        // A profile-based lookup (no table) is preserved raw.
        let mut p = vec![0, 1];
        p.extend(VersionedDescriptor::new(Descriptor::new("null").with("lookupType", en("colorLookupType", "abstractProfile"))).to_bytes());
        assert!(matches!(parse(b"clrL", &p, None, Channels::Rgb), Adjustment::Unsupported { .. }));
        assert!(matches!(parse(b"clrL", &[0, 1, 9], None, Channels::Rgb), Adjustment::Unsupported { .. }));
        assert!(matches!(parse(b"selc", &[0, 1, 0, 0], None, Channels::Rgb), Adjustment::Unsupported { .. }));
    }

    #[test]
    fn malformed_falls_back_to_unsupported() {
        assert!(matches!(parse(b"levl", &[0, 2, 1], None, Channels::Rgb), Adjustment::Unsupported { .. }));
        assert!(matches!(parse(b"curv", &[0, 0, 9], None, Channels::Rgb), Adjustment::Unsupported { .. }));
        assert!(matches!(parse(b"thrs", &[], None, Channels::Rgb), Adjustment::Unsupported { .. }));
        assert!(matches!(parse(b"blnc", &[0; 40], None, Channels::Rgb), Adjustment::Unsupported { .. }));
    }

    #[test]
    fn non_rgb_ignores_channel_records() {
        let lc = LevelsChannel { in_black: 0.1, ..Default::default() };
        let a = Adjustment::Levels { master: LevelsChannel::default(), per_channel: [lc.clone(), lc.clone(), lc] };
        let b = write(&a);
        match parse(&b[0].0, &b[0].1, None, Channels::Other) {
            Adjustment::Levels { per_channel, .. } => assert_eq!(per_channel, <[LevelsChannel; 3]>::default()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn gray_uses_first_channel_record_for_all() {
        let lc = LevelsChannel { in_black: 44.0 / 255.0, ..Default::default() };
        let a = Adjustment::Levels { master: LevelsChannel::default(), per_channel: [lc.clone(), Default::default(), Default::default()] };
        let b = write(&a);
        match parse(&b[0].0, &b[0].1, None, Channels::Gray) {
            Adjustment::Levels { per_channel, .. } => assert_eq!(per_channel, [lc.clone(), lc.clone(), lc]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn legacy_brit_without_cged() {
        let a = parse(b"brit", &[0, 10, 0xff, 0xf6, 0, 127, 0], None, Channels::Rgb);
        assert_eq!(a, Adjustment::BrightnessContrast { brightness: 10.0, contrast: -10.0, legacy: true });
    }
}
