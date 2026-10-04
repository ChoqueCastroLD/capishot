//! Synthetic raw-file generators for tests and fuzz seeds: a lossless-JPEG
//! (T.81 process 14) encoder, a small TIFF writer, and DNG / CR2 / TIFF-EP
//! builders. The files are written from the public specifications only and
//! are meant to exercise the decoder, not to be camera-accurate.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

// ---------------------------------------------------------------- lossless JPEG

/// Huffman code lengths assigned to categories in decreasing frequency
/// order. Kraft sum < 1 and no code is all ones.
const LENGTHS: [u8; 17] = [2, 3, 3, 3, 4, 4, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13];

fn category(d: i32) -> u32 {
    if d == 0 {
        0
    } else if d == 32768 {
        16
    } else {
        32 - d.unsigned_abs().leading_zeros()
    }
}

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, v: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                let b = self.acc as u8;
                self.out.push(b);
                if b == 0xFF {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    fn flush(&mut self) {
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put((1 << pad) - 1, pad);
        }
    }
}

/// Encodes `samples` (raster order, `components` interleaved, values below
/// `2^precision`) as a lossless JPEG with the given predictor (1–7) and an
/// optional restart interval in rows.
pub fn lj92_encode(samples: &[u16], width: usize, height: usize, components: usize, precision: u8, predictor: u8, restart_rows: Option<usize>) -> Vec<u8> {
    assert_eq!(samples.len(), width * height * components);
    let row = width * components;
    let interval = restart_rows.unwrap_or(usize::MAX);
    // Differences per sample, following T.81 H.1.2.1 prediction rules.
    let mut diffs = Vec::with_capacity(samples.len());
    let initial = 1i32 << (precision - 1);
    for y in 0..height {
        let first_line = y % interval == 0;
        for x in 0..width {
            for c in 0..components {
                let i = y * row + x * components + c;
                let px = if x == 0 {
                    if first_line { initial } else { i32::from(samples[i - row]) }
                } else if first_line {
                    i32::from(samples[i - components])
                } else {
                    let ra = i32::from(samples[i - components]);
                    let rb = i32::from(samples[i - row]);
                    let rc = i32::from(samples[i - row - components]);
                    match predictor {
                        1 => ra,
                        2 => rb,
                        3 => rc,
                        4 => ra + rb - rc,
                        5 => ra + ((rb - rc) >> 1),
                        6 => rb + ((ra - rc) >> 1),
                        _ => (ra + rb) >> 1,
                    }
                };
                let mut d = (i32::from(samples[i]) - px) & 0xFFFF;
                if d > 32768 {
                    d -= 65536;
                }
                diffs.push(d);
            }
        }
    }
    // Table: categories ordered by frequency get increasing code lengths.
    let mut freq = [0usize; 17];
    for &d in &diffs {
        freq[category(d) as usize] += 1;
    }
    let mut order: Vec<usize> = (0..17).collect();
    order.sort_by_key(|&c| std::cmp::Reverse(freq[c]));
    let mut len_of = [0u8; 17];
    for (rank, &c) in order.iter().enumerate() {
        len_of[c] = LENGTHS[rank];
    }
    let mut counts = [0u8; 16];
    let mut vals: Vec<u8> = Vec::new();
    let mut code_of = [(0u32, 0u32); 17];
    let mut code = 0u32;
    for len in 1..=16u8 {
        for c in 0..17 {
            if len_of[c] == len {
                counts[len as usize - 1] += 1;
                vals.push(c as u8);
                code_of[c] = (code, u32::from(len));
                code += 1;
            }
        }
        code <<= 1;
    }
    let mut out = vec![0xFF, 0xD8];
    // SOF3
    out.extend_from_slice(&[0xFF, 0xC3]);
    out.extend_from_slice(&((8 + 3 * components) as u16).to_be_bytes());
    out.push(precision);
    out.extend_from_slice(&(height as u16).to_be_bytes());
    out.extend_from_slice(&(width as u16).to_be_bytes());
    out.push(components as u8);
    for c in 0..components {
        out.extend_from_slice(&[c as u8 + 1, 0x11, 0]);
    }
    // DHT (one table, id 0)
    out.extend_from_slice(&[0xFF, 0xC4]);
    out.extend_from_slice(&((2 + 17 + vals.len()) as u16).to_be_bytes());
    out.push(0x00);
    out.extend_from_slice(&counts);
    out.extend_from_slice(&vals);
    if let Some(r) = restart_rows {
        out.extend_from_slice(&[0xFF, 0xDD, 0, 4]);
        out.extend_from_slice(&((r * width) as u16).to_be_bytes());
    }
    // SOS
    out.extend_from_slice(&[0xFF, 0xDA]);
    out.extend_from_slice(&((6 + 2 * components) as u16).to_be_bytes());
    out.push(components as u8);
    for c in 0..components {
        out.extend_from_slice(&[c as u8 + 1, 0x00]);
    }
    out.extend_from_slice(&[predictor, 0, 0]);
    let mut w = BitWriter { out, acc: 0, n: 0 };
    let mut rst = 0u8;
    for (i, &d) in diffs.iter().enumerate() {
        if i > 0 && i % (interval.saturating_mul(row)) == 0 {
            w.flush();
            w.out.extend_from_slice(&[0xFF, 0xD0 + rst]);
            rst = (rst + 1) % 8;
        }
        let s = category(d);
        let (c, l) = code_of[s as usize];
        w.put(c, l);
        if (1..16).contains(&s) {
            let extra = if d < 0 { (d - 1) as u32 & ((1 << s) - 1) } else { d as u32 };
            w.put(extra, s);
        }
    }
    w.flush();
    let mut out = w.out;
    out.extend_from_slice(&[0xFF, 0xD9]);
    out
}

// ---------------------------------------------------------------- TIFF writer

/// A TIFF entry value.
#[derive(Debug, Clone)]
pub enum Val {
    Byte(Vec<u8>),
    Ascii(String),
    Short(Vec<u16>),
    Long(Vec<u32>),
    Rational(Vec<(u32, u32)>),
    SRational(Vec<(i32, i32)>),
    Undefined(Vec<u8>),
    /// Offsets of these blobs (LONG).
    Blobs(Vec<usize>),
    /// Offsets of these IFDs (LONG), for SubIFDs / EXIF.
    Ifds(Vec<usize>),
    /// An UNDEFINED value whose bytes are this IFD (a maker note).
    IfdBytes(usize),
}

/// A TIFF file under construction.
#[derive(Debug, Clone, Default)]
pub struct TiffBuilder {
    pub big_endian: bool,
    /// Entries per IFD (sorted by tag on write).
    pub ifds: Vec<Vec<(u16, Val)>>,
    /// The main IFD chain, by index into `ifds`.
    pub chain: Vec<usize>,
    pub blobs: Vec<Vec<u8>>,
    /// Write a CR2 header pointing at this IFD as the raw IFD.
    pub cr2_raw_ifd: Option<usize>,
}

impl TiffBuilder {
    pub fn ifd(&mut self, entries: Vec<(u16, Val)>) -> usize {
        self.ifds.push(entries);
        self.ifds.len() - 1
    }

    pub fn blob(&mut self, data: Vec<u8>) -> usize {
        self.blobs.push(data);
        self.blobs.len() - 1
    }

    fn val_size(&self, v: &Val) -> usize {
        match v {
            Val::Byte(b) | Val::Undefined(b) => b.len(),
            Val::Ascii(s) => s.len() + 1,
            Val::Short(s) => 2 * s.len(),
            Val::Long(l) => 4 * l.len(),
            Val::Rational(r) => 8 * r.len(),
            Val::SRational(r) => 8 * r.len(),
            Val::Blobs(b) => 4 * b.len(),
            Val::Ifds(i) => 4 * i.len(),
            Val::IfdBytes(i) => self.ifd_size(*i),
        }
    }

    fn ifd_size(&self, i: usize) -> usize {
        2 + 12 * self.ifds[i].len() + 4
    }

    pub fn build(&self) -> Vec<u8> {
        let header = if self.cr2_raw_ifd.is_some() { 16 } else { 8 };
        // Layout: each IFD, then its overflow values; then the blobs.
        let mut pos = header;
        let mut ifd_pos = vec![0usize; self.ifds.len()];
        let mut val_pos: Vec<Vec<usize>> = vec![Vec::new(); self.ifds.len()];
        // Maker-note IFDs are written inside their owner's value area.
        let embedded: Vec<usize> = self.ifds.iter().flatten().filter_map(|(_, v)| if let Val::IfdBytes(i) = v { Some(*i) } else { None }).collect();
        for i in 0..self.ifds.len() {
            if embedded.contains(&i) {
                continue;
            }
            ifd_pos[i] = pos;
            pos += self.ifd_size(i);
            let mut vp = Vec::new();
            for (_, v) in self.sorted(i) {
                let size = self.val_size(&v);
                if size > 4 {
                    vp.push(pos);
                    if let Val::IfdBytes(e) = v {
                        ifd_pos[e] = pos;
                        // The embedded IFD's own values follow it.
                        let inner = pos + self.ifd_size(e);
                        let mut ip = Vec::new();
                        let mut q = inner;
                        for (_, iv) in self.sorted(e) {
                            let s = self.val_size(&iv);
                            if s > 4 {
                                ip.push(q);
                                q += s + (s & 1);
                            } else {
                                ip.push(0);
                            }
                        }
                        val_pos[e] = ip;
                        pos = q;
                    } else {
                        pos += size + (size & 1);
                    }
                } else {
                    vp.push(0);
                }
            }
            val_pos[i] = vp;
        }
        let mut blob_pos = Vec::new();
        for b in &self.blobs {
            blob_pos.push(pos);
            pos += b.len() + (b.len() & 1);
        }
        let mut out = vec![0u8; pos];
        let be = self.big_endian;
        let w16 = |o: &mut [u8], at: usize, v: u16| o[at..at + 2].copy_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() });
        let w32 = |o: &mut [u8], at: usize, v: u32| o[at..at + 4].copy_from_slice(&if be { v.to_be_bytes() } else { v.to_le_bytes() });
        out[0..4].copy_from_slice(if be { b"MM\0*" } else { b"II*\0" });
        w32(&mut out, 4, ifd_pos[self.chain[0]] as u32);
        if let Some(r) = self.cr2_raw_ifd {
            out[8..12].copy_from_slice(b"CR\x02\0");
            w32(&mut out, 12, ifd_pos[r] as u32);
        }
        for i in 0..self.ifds.len() {
            let at = ifd_pos[i];
            let entries = self.sorted(i);
            w16(&mut out, at, entries.len() as u16);
            for (k, (tag, v)) in entries.iter().enumerate() {
                let e = at + 2 + 12 * k;
                let (typ, count, bytes) = self.encode(v, &blob_pos, &ifd_pos);
                w16(&mut out, e, *tag);
                w16(&mut out, e + 2, typ);
                w32(&mut out, e + 4, count);
                if bytes.len() <= 4 {
                    out[e + 8..e + 8 + bytes.len()].copy_from_slice(&bytes);
                } else {
                    let vp = val_pos[i][k];
                    w32(&mut out, e + 8, vp as u32);
                    if !matches!(v, Val::IfdBytes(_)) {
                        out[vp..vp + bytes.len()].copy_from_slice(&bytes);
                    }
                }
            }
            let next = self.chain.iter().position(|&c| c == i).and_then(|p| self.chain.get(p + 1)).map(|&n| ifd_pos[n]).unwrap_or(0);
            w32(&mut out, at + 2 + 12 * entries.len(), next as u32);
        }
        for (b, &p) in self.blobs.iter().zip(&blob_pos) {
            out[p..p + b.len()].copy_from_slice(b);
        }
        out
    }

    fn sorted(&self, i: usize) -> Vec<(u16, Val)> {
        let mut e = self.ifds[i].clone();
        e.sort_by_key(|(t, _)| *t);
        e
    }

    fn encode(&self, v: &Val, blob_pos: &[usize], ifd_pos: &[usize]) -> (u16, u32, Vec<u8>) {
        let be = self.big_endian;
        let b16 = |v: u16| if be { v.to_be_bytes() } else { v.to_le_bytes() };
        let b32 = |v: u32| if be { v.to_be_bytes() } else { v.to_le_bytes() };
        match v {
            Val::Byte(b) => (1, b.len() as u32, b.clone()),
            Val::Undefined(b) => (7, b.len() as u32, b.clone()),
            Val::Ascii(s) => {
                let mut b = s.as_bytes().to_vec();
                b.push(0);
                (2, b.len() as u32, b)
            }
            Val::Short(s) => (3, s.len() as u32, s.iter().flat_map(|&v| b16(v)).collect()),
            Val::Long(l) => (4, l.len() as u32, l.iter().flat_map(|&v| b32(v)).collect()),
            Val::Rational(r) => (5, r.len() as u32, r.iter().flat_map(|&(n, d)| [b32(n), b32(d)].concat()).collect()),
            Val::SRational(r) => (10, r.len() as u32, r.iter().flat_map(|&(n, d)| [b32(n as u32), b32(d as u32)].concat()).collect()),
            Val::Blobs(b) => (4, b.len() as u32, b.iter().flat_map(|&i| b32(blob_pos[i] as u32)).collect()),
            Val::Ifds(i) => (4, i.len() as u32, i.iter().flat_map(|&k| b32(ifd_pos[k] as u32)).collect()),
            Val::IfdBytes(i) => (7, self.ifd_size(*i) as u32, vec![0; 8]),
        }
    }
}

fn srat(v: f64) -> (i32, i32) {
    ((v * 10000.0).round() as i32, 10000)
}

fn urat(v: f64) -> (u32, u32) {
    ((v * 10000.0).round() as u32, 10000)
}

// ---------------------------------------------------------------- DNG

/// How the DNG stores its raw data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DngStorage {
    /// One strip per `rows` rows, uncompressed at `bits` per sample (8, 12, 16…).
    Strips { rows: usize },
    /// Lossless-JPEG tiles of this size (2 components per JPEG row, as Adobe writes CFA tiles).
    Lj92Tiles { width: usize, height: usize },
    /// Lossless-JPEG strips of this many rows, 1 component.
    Lj92Strips { rows: usize },
}

/// A synthetic DNG.
#[derive(Debug, Clone)]
pub struct DngSpec {
    pub width: usize,
    pub height: usize,
    /// 1 (CFA) or 3 (LinearRaw).
    pub samples: usize,
    /// Row-major samples (`width * height * samples`).
    pub data: Vec<u16>,
    pub bits: u16,
    pub cfa: [u8; 4],
    pub storage: DngStorage,
    pub big_endian: bool,
    pub black: Vec<u32>,
    pub black_repeat: (u16, u16),
    pub white: u32,
    /// top, left, bottom, right
    pub active_area: Option<[u32; 4]>,
    /// origin (x, y), size (w, h), relative to the active area
    pub default_crop: Option<([u32; 2], [u32; 2])>,
    pub linearization: Option<Vec<u16>>,
    /// (illuminant code, ColorMatrix row-major)
    pub color_matrix1: Option<(u16, [f64; 9])>,
    pub color_matrix2: Option<(u16, [f64; 9])>,
    pub forward_matrix1: Option<[f64; 9]>,
    pub forward_matrix2: Option<[f64; 9]>,
    pub as_shot_neutral: Option<[f64; 3]>,
    pub baseline_exposure: Option<f64>,
    /// Raw OpcodeList2 bytes (see [`gain_map_opcode_list`]).
    pub opcode_list2: Option<Vec<u8>>,
    pub orientation: u16,
    pub make: String,
    pub model: String,
}

impl DngSpec {
    /// A CFA DNG with default metadata around `data` (RGGB, 16-bit, one strip).
    pub fn cfa(width: usize, height: usize, data: Vec<u16>) -> Self {
        DngSpec {
            width,
            height,
            samples: 1,
            data,
            bits: 16,
            cfa: [0, 1, 1, 2],
            storage: DngStorage::Strips { rows: height },
            big_endian: false,
            black: vec![0],
            black_repeat: (1, 1),
            white: 65535,
            active_area: None,
            default_crop: None,
            linearization: None,
            color_matrix1: None,
            color_matrix2: None,
            forward_matrix1: None,
            forward_matrix2: None,
            as_shot_neutral: None,
            baseline_exposure: None,
            orientation: 1,
            opcode_list2: None,
            make: "Photocraft".into(),
            model: "Synthetic".into(),
        }
    }

    fn pack(&self, rows: std::ops::Range<usize>) -> Vec<u8> {
        let n = self.width * self.samples;
        let vals = &self.data[rows.start * n..rows.end * n];
        match self.bits {
            8 => vals.iter().map(|&v| v as u8).collect(),
            16 => vals.iter().flat_map(|&v| if self.big_endian { v.to_be_bytes() } else { v.to_le_bytes() }).collect(),
            b => {
                // MSB-first bit packing, each row padded to a byte.
                let mut out = Vec::new();
                for row in vals.chunks(n) {
                    let mut acc: u64 = 0;
                    let mut have = 0u32;
                    for &v in row {
                        acc = (acc << b) | u64::from(v);
                        have += u32::from(b);
                        while have >= 8 {
                            have -= 8;
                            out.push((acc >> have) as u8);
                        }
                    }
                    if have > 0 {
                        out.push((acc << (8 - have)) as u8);
                    }
                }
                out
            }
        }
    }

    pub fn build(&self) -> Vec<u8> {
        let mut t = TiffBuilder { big_endian: self.big_endian, ..Default::default() };
        let mut raw: Vec<(u16, Val)> = vec![
            (254, Val::Long(vec![0])),
            (256, Val::Long(vec![self.width as u32])),
            (257, Val::Long(vec![self.height as u32])),
            (258, Val::Short(vec![self.bits; self.samples])),
            (262, Val::Short(vec![if self.samples == 1 { 32803 } else { 34892 }])),
            (277, Val::Short(vec![self.samples as u16])),
            (284, Val::Short(vec![1])),
            (50717, Val::Long(vec![self.white; self.samples])),
            (50713, Val::Short(vec![self.black_repeat.0, self.black_repeat.1])),
            (50714, Val::Long(self.black.clone())),
        ];
        if self.samples == 1 {
            raw.push((33421, Val::Short(vec![2, 2])));
            raw.push((33422, Val::Byte(self.cfa.to_vec())));
            raw.push((50710, Val::Byte(vec![0, 1, 2])));
            raw.push((50711, Val::Short(vec![1])));
        }
        match self.storage {
            DngStorage::Strips { rows } => {
                let mut offs = Vec::new();
                let mut lens = Vec::new();
                for y in (0..self.height).step_by(rows) {
                    let b = self.pack(y..(y + rows).min(self.height));
                    lens.push(b.len() as u32);
                    offs.push(t.blob(b));
                }
                raw.push((259, Val::Short(vec![1])));
                raw.push((278, Val::Long(vec![rows as u32])));
                raw.push((273, Val::Blobs(offs)));
                raw.push((279, Val::Long(lens)));
            }
            DngStorage::Lj92Strips { rows } => {
                let mut offs = Vec::new();
                let mut lens = Vec::new();
                for y in (0..self.height).step_by(rows) {
                    let r = (y + rows).min(self.height) - y;
                    let n = self.width * self.samples;
                    let j = lj92_encode(&self.data[y * n..(y + r) * n], self.width, r, self.samples, self.bits as u8, 1, None);
                    lens.push(j.len() as u32);
                    offs.push(t.blob(j));
                }
                raw.push((259, Val::Short(vec![7])));
                raw.push((278, Val::Long(vec![rows as u32])));
                raw.push((273, Val::Blobs(offs)));
                raw.push((279, Val::Long(lens)));
            }
            DngStorage::Lj92Tiles { width: tw, height: th } => {
                let mut offs = Vec::new();
                let mut lens = Vec::new();
                let s = self.samples;
                for ty in (0..self.height).step_by(th) {
                    for tx in (0..self.width).step_by(tw) {
                        // Edge tiles are padded by repeating the last row / column.
                        let mut tile = vec![0u16; tw * th * s];
                        for y in 0..th {
                            for x in 0..tw {
                                let sy = (ty + y).min(self.height - 1);
                                let sx = (tx + x).min(self.width - 1);
                                for c in 0..s {
                                    tile[(y * tw + x) * s + c] = self.data[(sy * self.width + sx) * s + c];
                                }
                            }
                        }
                        // CFA tiles as Adobe writes them: half width, 2 components.
                        let j = if s == 1 && tw % 2 == 0 {
                            lj92_encode(&tile, tw / 2, th, 2, self.bits as u8, 1, None)
                        } else {
                            lj92_encode(&tile, tw, th, s, self.bits as u8, 1, None)
                        };
                        lens.push(j.len() as u32);
                        offs.push(t.blob(j));
                    }
                }
                raw.push((259, Val::Short(vec![7])));
                raw.push((322, Val::Long(vec![tw as u32])));
                raw.push((323, Val::Long(vec![th as u32])));
                raw.push((324, Val::Blobs(offs)));
                raw.push((325, Val::Long(lens)));
            }
        }
        if let Some(a) = self.active_area {
            raw.push((50829, Val::Long(a.to_vec())));
        }
        if let Some((o, s)) = self.default_crop {
            raw.push((50719, Val::Long(o.to_vec())));
            raw.push((50720, Val::Long(s.to_vec())));
        }
        if let Some(o) = &self.opcode_list2 {
            raw.push((51009, Val::Undefined(o.clone())));
        }
        if let Some(l) = &self.linearization {
            raw.push((50712, Val::Short(l.clone())));
        }
        let raw_ifd = t.ifd(raw);
        // IFD0: a tiny RGB thumbnail plus the DNG metadata.
        let thumb = t.blob(vec![128; 3 * 4]);
        let mut ifd0: Vec<(u16, Val)> = vec![
            (254, Val::Long(vec![1])),
            (256, Val::Long(vec![2])),
            (257, Val::Long(vec![2])),
            (258, Val::Short(vec![8, 8, 8])),
            (259, Val::Short(vec![1])),
            (262, Val::Short(vec![2])),
            (271, Val::Ascii(self.make.clone())),
            (272, Val::Ascii(self.model.clone())),
            (273, Val::Blobs(vec![thumb])),
            (274, Val::Short(vec![self.orientation])),
            (277, Val::Short(vec![3])),
            (278, Val::Long(vec![2])),
            (279, Val::Long(vec![12])),
            (330, Val::Ifds(vec![raw_ifd])),
            (50706, Val::Byte(vec![1, 4, 0, 0])),
            (50708, Val::Ascii(format!("{} {}", self.make, self.model))),
        ];
        let m = |v: &[f64; 9]| Val::SRational(v.iter().map(|&x| srat(x)).collect());
        if let Some((ill, cm)) = &self.color_matrix1 {
            ifd0.push((50721, m(cm)));
            ifd0.push((50778, Val::Short(vec![*ill])));
        }
        if let Some((ill, cm)) = &self.color_matrix2 {
            ifd0.push((50722, m(cm)));
            ifd0.push((50779, Val::Short(vec![*ill])));
        }
        if let Some(f) = &self.forward_matrix1 {
            ifd0.push((50964, m(f)));
        }
        if let Some(f) = &self.forward_matrix2 {
            ifd0.push((50965, m(f)));
        }
        if let Some(n) = self.as_shot_neutral {
            ifd0.push((50728, Val::Rational(n.iter().map(|&v| urat(v)).collect())));
        }
        if let Some(b) = self.baseline_exposure {
            ifd0.push((50730, Val::SRational(vec![srat(b)])));
        }
        let i0 = t.ifd(ifd0);
        t.chain = vec![i0];
        t.build()
    }
}

/// An OpcodeList2 with one GainMap over active-area rectangle `area` (top,
/// left, bottom, right), every `pitch` rows and columns, from a 2×2 grid of
/// gains spanning the image (relative coordinates 0..1).
pub fn gain_map_opcode_list(area: [u32; 4], pitch: u32, gains: [[f32; 2]; 2]) -> Vec<u8> {
    let mut p = Vec::new();
    for v in [area[0], area[1], area[2], area[3], 0, 1, pitch, pitch, 2, 2] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    for v in [1.0f64, 1.0, 0.0, 0.0] {
        p.extend_from_slice(&v.to_be_bytes());
    }
    p.extend_from_slice(&1u32.to_be_bytes());
    for g in gains.iter().flatten() {
        p.extend_from_slice(&g.to_bits().to_be_bytes());
    }
    let mut b = 1u32.to_be_bytes().to_vec();
    for v in [9u32, 0x0103_0000, 1, p.len() as u32] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&p);
    b
}

// ---------------------------------------------------------------- CR2

/// A synthetic CR2: one lossless JPEG with `components` components, cut into
/// vertical slices, plus a Canon maker note with SensorInfo and ColorData.
#[derive(Debug, Clone)]
pub struct Cr2Spec {
    pub width: usize,
    pub height: usize,
    /// Sensor samples, row-major, RGGB from (`left`, `top`).
    pub data: Vec<u16>,
    pub precision: u8,
    pub components: usize,
    /// Slice widths (all but the last equal); empty = no slice tag.
    pub slices: Vec<usize>,
    /// Image borders: left, top, right, bottom (inclusive), as in SensorInfo.
    pub borders: Option<[u16; 4]>,
    /// As-shot RGGB levels written to ColorData at word offset 0x3F.
    pub wb_rggb: Option<[u16; 4]>,
    pub orientation: u16,
}

impl Cr2Spec {
    pub fn build(&self) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        // Slice the sensor data into the JPEG sample order.
        let widths: Vec<usize> = if self.slices.is_empty() { vec![w] } else { self.slices.clone() };
        assert_eq!(widths.iter().sum::<usize>(), w);
        let mut seq = Vec::with_capacity(w * h);
        let mut x0 = 0;
        for &sw in &widths {
            for y in 0..h {
                seq.extend_from_slice(&self.data[y * w + x0..y * w + x0 + sw]);
            }
            x0 += sw;
        }
        let c = self.components;
        assert_eq!(w % c, 0);
        let jpeg = lj92_encode(&seq, w / c, h, c, self.precision, 1, None);
        let mut t = TiffBuilder::default();
        let jb = t.blob(jpeg.clone());
        let mut raw = vec![(259, Val::Short(vec![6])), (273, Val::Blobs(vec![jb])), (279, Val::Long(vec![jpeg.len() as u32]))];
        if !self.slices.is_empty() {
            let n = widths.len() - 1;
            raw.push((50752, Val::Short(vec![n as u16, widths[0] as u16, widths[n] as u16])));
        }
        let raw_ifd = t.ifd(raw);
        let mut mn: Vec<(u16, Val)> = Vec::new();
        if let Some([l, tp, r, b]) = self.borders {
            mn.push((0x00E0, Val::Short(vec![34, w as u16, h as u16, 0, 0, l, tp, r, b, 0, 0, 0, 0, 0, 0, 0, 0])));
        }
        if let Some(wb) = self.wb_rggb {
            let mut cd = vec![0u16; 1273];
            cd[0x3F..0x43].copy_from_slice(&wb);
            mn.push((0x4001, Val::Short(cd)));
        }
        let mut exif: Vec<(u16, Val)> = vec![(33434, Val::Rational(vec![(1, 100)]))];
        if !mn.is_empty() {
            let mi = t.ifd(mn);
            exif.push((37500, Val::IfdBytes(mi)));
        }
        let exif_ifd = t.ifd(exif);
        let thumb = t.blob(vec![128; 12]);
        let ifd0 = t.ifd(vec![
            (256, Val::Long(vec![2])),
            (257, Val::Long(vec![2])),
            (259, Val::Short(vec![1])),
            (262, Val::Short(vec![2])),
            (271, Val::Ascii("Canon".into())),
            (272, Val::Ascii("Canon EOS Synthetic".into())),
            (273, Val::Blobs(vec![thumb])),
            (274, Val::Short(vec![self.orientation])),
            (277, Val::Short(vec![3])),
            (279, Val::Long(vec![12])),
            (34665, Val::Ifds(vec![exif_ifd])),
        ]);
        let ifd1 = t.ifd(vec![(256, Val::Long(vec![1]))]);
        let ifd2 = t.ifd(vec![(256, Val::Long(vec![1]))]);
        t.chain = vec![ifd0, ifd1, ifd2, raw_ifd];
        t.cr2_raw_ifd = Some(raw_ifd);
        t.build()
    }
}

// ---------------------------------------------------------------- TIFF/EP

/// A synthetic uncompressed TIFF/EP raw (NEF / ARW-like): IFD0 with Make, the
/// CFA image in a SubIFD with 16-bit samples.
pub fn tiff_ep(make: &str, width: usize, height: usize, data: &[u16], cfa: [u8; 4], bits: u16, extra_raw_tags: Vec<(u16, Val)>) -> Vec<u8> {
    let mut t = TiffBuilder::default();
    let strip = t.blob(data.iter().flat_map(|v| v.to_le_bytes()).collect());
    let mut raw = vec![
        (254, Val::Long(vec![0])),
        (256, Val::Long(vec![width as u32])),
        (257, Val::Long(vec![height as u32])),
        (258, Val::Short(vec![bits])),
        (259, Val::Short(vec![1])),
        (262, Val::Short(vec![32803])),
        (273, Val::Blobs(vec![strip])),
        (277, Val::Short(vec![1])),
        (278, Val::Long(vec![height as u32])),
        (279, Val::Long(vec![(data.len() * 2) as u32])),
        (33421, Val::Short(vec![2, 2])),
        (33422, Val::Byte(cfa.to_vec())),
    ];
    raw.extend(extra_raw_tags);
    let raw_ifd = t.ifd(raw);
    let thumb = t.blob(vec![128; 12]);
    let ifd0 = t.ifd(vec![
        (254, Val::Long(vec![1])),
        (256, Val::Long(vec![2])),
        (257, Val::Long(vec![2])),
        (259, Val::Short(vec![1])),
        (262, Val::Short(vec![2])),
        (271, Val::Ascii(make.into())),
        (273, Val::Blobs(vec![thumb])),
        (277, Val::Short(vec![3])),
        (279, Val::Long(vec![12])),
        (330, Val::Ifds(vec![raw_ifd])),
    ]);
    t.chain = vec![ifd0];
    t.build()
}

// ---------------------------------------------------------------- scenes

/// A smooth, colourful synthetic scene as linear RGB in 0..1.
pub fn scene(width: usize, height: usize) -> Vec<[f32; 3]> {
    (0..width * height)
        .map(|i| {
            let (x, y) = ((i % width) as f32 / width.max(1) as f32, (i / width) as f32 / height.max(1) as f32);
            [0.1 + 0.7 * x, 0.15 + 0.5 * (1.0 - y) * x + 0.2 * y, 0.6 - 0.4 * y + 0.2 * x]
        })
        .collect()
}

/// Mosaics linear RGB through a 2×2 CFA (`cfa` row-major colours) into
/// sensor values `black + v * (white - black)`.
pub fn mosaic(rgb: &[[f32; 3]], width: usize, cfa: [u8; 4], black: u16, white: u16) -> Vec<u16> {
    rgb.iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = (i % width, i / width);
            let c = cfa[(y & 1) * 2 + (x & 1)] as usize;
            let v = p[c].clamp(0.0, 1.0);
            (f32::from(black) + v * f32::from(white - black)).round() as u16
        })
        .collect()
}
