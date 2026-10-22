//! Incremental safetensors parsing: feed a file in chunks as it streams in (from the network or
//! Cache Storage) and get each tensor as soon as its bytes have arrived, so a 450 MB shard never
//! sits whole in the wasm heap (peak: the largest tensor plus one chunk).
//!
//! Tensors must be laid out in offset order without gaps or overlaps (as every writer does);
//! anything else is an error, like a header that isn't JSON or sizes that don't add up.

use std::collections::HashMap;

use crate::{Result, SdError};

/// Largest accepted header (the UNet's is ~100 KB).
const MAX_HEADER: u64 = 16 << 20;

/// One complete tensor.
#[derive(Debug, Clone, PartialEq)]
pub struct RawTensor {
    pub name: String,
    /// safetensors dtype name (`F16`, `F32`, `BF16`, `U8`…).
    pub dtype: String,
    pub shape: Vec<usize>,
    pub data: Vec<u8>,
}

#[derive(Debug)]
struct Entry {
    name: String,
    dtype: String,
    shape: Vec<usize>,
    start: u64,
    end: u64,
}

/// See the module docs.
#[derive(Debug, Default)]
pub struct StreamParser {
    buf: Vec<u8>,
    header_len: Option<u64>,
    entries: Vec<Entry>,
    next: usize,
    /// Bytes of the data section consumed so far.
    pos: u64,
    metadata: HashMap<String, String>,
    header_done: bool,
}

fn bad(m: impl Into<String>) -> SdError {
    SdError::Weights(m.into())
}

fn dtype_size(d: &str) -> Option<u64> {
    Some(match d {
        "F64" | "I64" | "U64" => 8,
        "F32" | "I32" | "U32" => 4,
        "F16" | "BF16" | "I16" | "U16" => 2,
        "U8" | "I8" | "BOOL" => 1,
        _ => return None,
    })
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// The header's `__metadata__` (empty until the header has arrived).
    pub fn metadata(&self) -> &HashMap<String, String> {
        &self.metadata
    }

    /// Whether every tensor has been returned.
    pub fn finished(&self) -> bool {
        self.header_done && self.next == self.entries.len()
    }

    /// Adds a chunk and returns the tensors it completed, in file order.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<RawTensor>> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        if !self.header_done {
            if self.header_len.is_none() && self.buf.len() >= 8 {
                let mut n = [0u8; 8];
                n.copy_from_slice(&self.buf[..8]);
                let n = u64::from_le_bytes(n);
                if n == 0 || n > MAX_HEADER {
                    return Err(bad(format!("safetensors header of {n} bytes")));
                }
                self.header_len = Some(n);
            }
            let Some(n) = self.header_len else { return Ok(out) };
            let total = 8 + n as usize;
            if self.buf.len() < total {
                return Ok(out);
            }
            let head: Vec<u8> = self.buf.drain(..total).collect();
            self.parse_header(head.get(8..).unwrap_or_default())?;
            self.header_done = true;
        }
        while let Some(e) = self.entries.get(self.next) {
            if e.start != self.pos {
                return Err(bad(format!("`{}` doesn't start where the previous tensor ends", e.name)));
            }
            let len = (e.end - e.start) as usize;
            if self.buf.len() < len {
                break;
            }
            let data: Vec<u8> = self.buf.drain(..len).collect();
            out.push(RawTensor { name: e.name.clone(), dtype: e.dtype.clone(), shape: e.shape.clone(), data });
            self.pos = e.end;
            self.next += 1;
        }
        if self.finished() && !self.buf.is_empty() {
            return Err(bad("trailing bytes after the last tensor"));
        }
        Ok(out)
    }

    fn parse_header(&mut self, raw: &[u8]) -> Result<()> {
        let v: serde_json::Value = serde_json::from_slice(raw).map_err(|e| bad(format!("safetensors header: {e}")))?;
        let obj = v.as_object().ok_or_else(|| bad("safetensors header isn't an object"))?;
        for (k, v) in obj {
            if k == "__metadata__" {
                if let Some(m) = v.as_object() {
                    self.metadata = m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect();
                }
                continue;
            }
            let dtype = v.get("dtype").and_then(|d| d.as_str()).ok_or_else(|| bad(format!("`{k}`: no dtype")))?.to_string();
            let shape: Vec<usize> = v
                .get("shape")
                .and_then(|s| s.as_array())
                .ok_or_else(|| bad(format!("`{k}`: no shape")))?
                .iter()
                .map(|d| d.as_u64().map(|d| d as usize))
                .collect::<Option<_>>()
                .ok_or_else(|| bad(format!("`{k}`: bad shape")))?;
            let off = v.get("data_offsets").and_then(|o| o.as_array()).filter(|o| o.len() == 2).ok_or_else(|| bad(format!("`{k}`: bad offsets")))?;
            let (start, end) = (off[0].as_u64().unwrap_or(u64::MAX), off[1].as_u64().unwrap_or(0));
            let size = dtype_size(&dtype).ok_or_else(|| bad(format!("`{k}`: unknown dtype {dtype}")))?;
            let n = shape.iter().try_fold(1u64, |a, &d| a.checked_mul(d as u64)).and_then(|n| n.checked_mul(size));
            if end < start || n != Some(end - start) {
                return Err(bad(format!("`{k}`: offsets don't match its shape")));
            }
            self.entries.push(Entry { name: k.clone(), dtype, shape, start, end });
        }
        self.entries.sort_by_key(|e| e.start);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> Vec<u8> {
        let header =
            r#"{"__metadata__":{"arch":"x"},"b":{"dtype":"F16","shape":[2],"data_offsets":[4,8]},"a":{"dtype":"U8","shape":[4],"data_offsets":[0,4]}}"#;
        let mut f = (header.len() as u64).to_le_bytes().to_vec();
        f.extend_from_slice(header.as_bytes());
        f.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        f
    }

    #[test]
    fn any_chunking_gives_the_same_tensors() {
        let f = file();
        for chunk in [1, 3, 7, 1000] {
            let mut p = StreamParser::new();
            let mut got = Vec::new();
            for c in f.chunks(chunk) {
                got.extend(p.push(c).unwrap_or_else(|e| panic!("{e}")));
            }
            assert!(p.finished());
            assert_eq!(p.metadata().get("arch").map(String::as_str), Some("x"));
            assert_eq!(got.len(), 2);
            assert_eq!((got[0].name.as_str(), got[0].data.as_slice()), ("a", &[1u8, 2, 3, 4][..]));
            assert_eq!((got[1].name.as_str(), got[1].shape.as_slice(), got[1].data.as_slice()), ("b", &[2usize][..], &[5u8, 6, 7, 8][..]));
        }
    }

    #[test]
    fn broken_files_are_errors() {
        let mut f = file();
        f.push(9);
        let mut p = StreamParser::new();
        assert!(p.push(&f).is_err());
        let mut p = StreamParser::new();
        assert!(p.push(&[255; 16]).is_err());
        let header = r#"{"a":{"dtype":"F16","shape":[3],"data_offsets":[0,4]}}"#;
        let mut g = (header.len() as u64).to_le_bytes().to_vec();
        g.extend_from_slice(header.as_bytes());
        assert!(StreamParser::new().push(&g).is_err());
    }
}
