//! Conversions between PSD planar big-endian channel data and interleaved
//! native-endian surface bytes.

use photocraft_color::SampleType;

/// PSD depth (bits) for a sample type.
pub fn psd_depth(s: SampleType) -> u16 {
    match s {
        SampleType::U8 => 8,
        SampleType::U16 => 16,
        SampleType::F32 => 32,
    }
}

/// Sample type for a PSD depth (1-bit is expanded to 8).
pub fn sample_for_depth(depth: u16) -> SampleType {
    match depth {
        16 => SampleType::U16,
        32 => SampleType::F32,
        _ => SampleType::U8,
    }
}

fn invert_sample(bytes: &mut [u8], s: SampleType) {
    match s {
        SampleType::U8 => bytes[0] = 255 - bytes[0],
        SampleType::U16 => {
            let v = u16::from_ne_bytes([bytes[0], bytes[1]]);
            bytes.copy_from_slice(&(65535 - v).to_ne_bytes());
        }
        SampleType::F32 => {
            let v = f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            bytes.copy_from_slice(&(1.0 - v).to_ne_bytes());
        }
    }
}

/// Interleaves planes (big-endian samples, `n` pixels each). `None` planes
/// are filled with `fill[c]` (a native-endian encoded sample). Channels whose
/// `invert[c]` is set are inverted (PSD stores CMYK as `max - ink`).
pub fn interleave(planes: &[Option<&[u8]>], fill: &[Vec<u8>], n: usize, s: SampleType, invert: &[bool]) -> Vec<u8> {
    let bps = s.bytes();
    let ch = planes.len();
    let mut out = vec![0u8; n * ch * bps];
    for (c, plane) in planes.iter().enumerate() {
        for i in 0..n {
            let dst = &mut out[(i * ch + c) * bps..(i * ch + c + 1) * bps];
            match plane {
                Some(p) if p.len() >= (i + 1) * bps => {
                    let src = &p[i * bps..(i + 1) * bps];
                    for k in 0..bps {
                        dst[k] = src[bps - 1 - k];
                    }
                    if cfg!(target_endian = "big") {
                        dst.copy_from_slice(src);
                    }
                    if invert[c] {
                        invert_sample(dst, s);
                    }
                }
                _ => dst.copy_from_slice(&fill[c]),
            }
        }
    }
    out
}

/// Splits interleaved native-endian bytes into big-endian planes.
pub fn deinterleave(bytes: &[u8], ch: usize, s: SampleType, invert: &[bool]) -> Vec<Vec<u8>> {
    let bps = s.bytes();
    let n = bytes.len() / (ch * bps).max(1);
    if s == SampleType::U8 {
        // Fast path: one plane per thread, no per-sample copies.
        return par_map((0..ch).collect(), |c| {
            let it = bytes.iter().skip(c).step_by(ch).take(n);
            if invert[c] { it.map(|v| 255 - v).collect() } else { it.copied().collect() }
        });
    }
    let mut planes = vec![Vec::with_capacity(n * bps); ch];
    let mut tmp = [0u8; 4];
    for i in 0..n {
        for (c, plane) in planes.iter_mut().enumerate() {
            let src = &bytes[(i * ch + c) * bps..(i * ch + c + 1) * bps];
            tmp[..bps].copy_from_slice(src);
            if invert[c] {
                invert_sample(&mut tmp[..bps], s);
            }
            if cfg!(target_endian = "big") {
                plane.extend_from_slice(&tmp[..bps]);
            } else {
                plane.extend(tmp[..bps].iter().rev());
            }
        }
    }
    planes
}

/// Native-endian encoding of the maximum sample value.
pub fn max_sample(s: SampleType) -> Vec<u8> {
    match s {
        SampleType::U8 => vec![255],
        SampleType::U16 => 65535u16.to_ne_bytes().to_vec(),
        SampleType::F32 => 1.0f32.to_ne_bytes().to_vec(),
    }
}

/// Native-endian encoding of zero.
pub fn zero_sample(s: SampleType) -> Vec<u8> {
    vec![0; s.bytes()]
}

/// Quantizes a normalized value to big-endian bytes.
pub fn encode_be(v: f32, s: SampleType, out: &mut Vec<u8>) {
    match s {
        SampleType::U8 => out.push((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8),
        SampleType::U16 => out.extend_from_slice(&((v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16).to_be_bytes()),
        SampleType::F32 => out.extend_from_slice(&v.to_be_bytes()),
    }
}

/// Decodes a big-endian sample at `index` to a normalized value.
#[cfg(test)]
pub fn decode_be(bytes: &[u8], index: usize, s: SampleType) -> f32 {
    match s {
        SampleType::U8 => f32::from(bytes[index]) / 255.0,
        SampleType::U16 => f32::from(u16::from_be_bytes([bytes[index * 2], bytes[index * 2 + 1]])) / 65535.0,
        SampleType::F32 => {
            let o = index * 4;
            f32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
        }
    }
}

/// Photoshop stores the merged image of a transparent document matted
/// against white: `m = c·a + w·(1 − a)` per stored channel, where `w` is the
/// model's white. These convert between straight and matted values.
pub fn matte(c: f32, a: f32, white: f32) -> f32 {
    c * a + white * (1.0 - a)
}

/// Inverse of [`matte`] (returns `white` where alpha is 0).
pub fn unmatte(m: f32, a: f32, white: f32) -> f32 {
    if a <= 0.0 { white } else { ((m - white * (1.0 - a)) / a).clamp(0.0, 1.0) }
}

/// `items.map(f)` on scoped threads (one per item; callers pass a handful of channels or
/// bands), in order; sequential on wasm or for a single item.
pub fn par_map<T: Send, R: Send>(items: Vec<T>, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    if cfg!(target_arch = "wasm32") || items.len() < 2 {
        return items.into_iter().map(f).collect();
    }
    std::thread::scope(|sc| {
        let f = &f;
        let hs: Vec<_> = items.into_iter().map(|t| sc.spawn(move || f(t))).collect();
        // Join every worker first (an unjoined panicked thread would make the scope panic),
        // then hand a worker's panic to the caller exactly as the sequential path would, so
        // the shell's last-resort guard reports it instead of the scope aborting the join.
        let joined: Vec<_> = hs.into_iter().map(|h| h.join()).collect();
        joined.into_iter().map(|r| r.unwrap_or_else(|p| std::panic::resume_unwind(p))).collect()
    })
}

/// `0..n` split into about one band per core.
pub fn bands(n: usize) -> Vec<std::ops::Range<usize>> {
    let k = if cfg!(target_arch = "wasm32") { 1 } else { std::thread::available_parallelism().map_or(1, |v| v.get()).clamp(1, 32) };
    let step = n.div_ceil(k).max(1);
    (0..n).step_by(step).map(|a| a..(a + step).min(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_roundtrip_all_depths() {
        for s in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let bps = s.bytes();
            let planes: Vec<Vec<u8>> = (0..3).map(|c| (0..4 * bps).map(|i| (i * 7 + c * 3) as u8).collect()).collect();
            let refs: Vec<Option<&[u8]>> = planes.iter().map(|p| Some(&p[..])).collect();
            let fill = vec![zero_sample(s); 3];
            let inv = [false, true, false];
            let il = interleave(&refs, &fill, 4, s, &inv);
            let back = deinterleave(&il, 3, s, &inv);
            if s == SampleType::F32 {
                // 1 - (1 - v) may not be exact for floats; only check non-inverted planes.
                assert_eq!(back[0], planes[0]);
                assert_eq!(back[2], planes[2]);
            } else {
                assert_eq!(back, planes);
            }
        }
    }

    #[test]
    fn missing_planes_use_fill() {
        let il = interleave(&[None, Some(&[5, 6])], &[vec![9], vec![0]], 2, SampleType::U8, &[false, false]);
        assert_eq!(il, vec![9, 5, 9, 6]);
    }

    #[test]
    fn be_codec() {
        let mut v = Vec::new();
        encode_be(1.0, SampleType::U16, &mut v);
        assert_eq!(v, vec![0xff, 0xff]);
        assert_eq!(decode_be(&v, 0, SampleType::U16), 1.0);
        let mut f = Vec::new();
        encode_be(2.5, SampleType::F32, &mut f);
        assert_eq!(decode_be(&f, 0, SampleType::F32), 2.5);
    }
}
