//! Model weights from safetensors bytes (F32, F16 or BF16), converted to f32.
//!
//! The format: an 8-byte little-endian header length, a JSON header naming each tensor's dtype,
//! shape and byte range, then the raw data. Everything is validated: no offset, shape or length
//! from the file is trusted.

use std::collections::BTreeMap;

use crate::{AiError, Result};

/// Largest header accepted.
const MAX_HEADER: u64 = 16 << 20;

/// One tensor: its shape and its values in row-major order.
#[derive(Clone, Debug, PartialEq)]
pub struct WeightTensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

/// All tensors of a model file plus its `__metadata__` strings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Weights {
    pub tensors: BTreeMap<String, WeightTensor>,
    pub metadata: BTreeMap<String, String>,
}

impl Weights {
    /// Parses a safetensors file.
    pub fn from_safetensors(bytes: &[u8]) -> Result<Self> {
        let bad = |m: &str| AiError::Weights(m.to_string());
        let len_bytes: [u8; 8] = bytes.get(..8).and_then(|b| b.try_into().ok()).ok_or_else(|| bad("file too short"))?;
        let header_len = u64::from_le_bytes(len_bytes);
        if header_len > MAX_HEADER {
            return Err(bad("header too large"));
        }
        let header_end = 8usize.checked_add(header_len as usize).ok_or_else(|| bad("header length"))?;
        let header = bytes.get(8..header_end).ok_or_else(|| bad("truncated header"))?;
        let data = bytes.get(header_end..).unwrap_or(&[]);
        let json: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(header).map_err(|e| AiError::Weights(format!("header: {e}")))?;
        let mut out = Weights::default();
        for (name, v) in json {
            if name == "__metadata__" {
                if let Some(m) = v.as_object() {
                    for (k, v) in m {
                        if let Some(s) = v.as_str() {
                            out.metadata.insert(k.clone(), s.to_string());
                        }
                    }
                }
                continue;
            }
            let dtype = v.get("dtype").and_then(|d| d.as_str()).ok_or_else(|| AiError::Weights(format!("{name}: no dtype")))?;
            let shape: Vec<usize> = v
                .get("shape")
                .and_then(|s| s.as_array())
                .ok_or_else(|| AiError::Weights(format!("{name}: no shape")))?
                .iter()
                .map(|d| d.as_u64().and_then(|d| usize::try_from(d).ok()))
                .collect::<Option<_>>()
                .ok_or_else(|| AiError::Weights(format!("{name}: bad shape")))?;
            let offs = v.get("data_offsets").and_then(|o| o.as_array()).ok_or_else(|| AiError::Weights(format!("{name}: no offsets")))?;
            let (a, b) = match (offs.first().and_then(|o| o.as_u64()), offs.get(1).and_then(|o| o.as_u64())) {
                (Some(a), Some(b)) if a <= b => (a as usize, b as usize),
                _ => return Err(AiError::Weights(format!("{name}: bad offsets"))),
            };
            let raw = data.get(a..b).ok_or_else(|| AiError::Weights(format!("{name}: data out of range")))?;
            // Integer tensors are bookkeeping, not weights (spandrel's `upsampler.MetaUpsample` in
            // the IllustrationJaNai V3 releases): skip them.
            if matches!(dtype, "U8" | "I8" | "U16" | "I16" | "U32" | "I32" | "U64" | "I64" | "BOOL") {
                continue;
            }
            let count = shape.iter().try_fold(1usize, |n, &d| n.checked_mul(d)).ok_or_else(|| AiError::Weights(format!("{name}: shape overflow")))?;
            let (size, values): (usize, Vec<f32>) = match dtype {
                "F32" => (4, raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()),
                "F16" => (2, raw.as_chunks::<2>().0.iter().map(|c| half::f16::from_le_bytes(*c).to_f32()).collect()),
                "BF16" => (2, raw.as_chunks::<2>().0.iter().map(|c| half::bf16::from_le_bytes(*c).to_f32()).collect()),
                other => return Err(AiError::Weights(format!("{name}: unsupported dtype {other}"))),
            };
            if count.checked_mul(size) != Some(raw.len()) {
                return Err(AiError::Weights(format!("{name}: {} bytes for shape {shape:?}", raw.len())));
            }
            out.tensors.insert(name, WeightTensor { shape, data: values });
        }
        Ok(out)
    }

    /// A tensor that must exist with exactly this shape.
    pub fn get(&self, name: &str, shape: &[usize]) -> Result<&WeightTensor> {
        let t = self.tensors.get(name).ok_or_else(|| AiError::Weights(format!("missing tensor {name}")))?;
        if t.shape != shape {
            return Err(AiError::Weights(format!("{name} has shape {:?}, expected {shape:?}", t.shape)));
        }
        Ok(t)
    }

    /// The shape of a tensor, if present.
    pub fn shape(&self, name: &str) -> Option<&[usize]> {
        self.tensors.get(name).map(|t| t.shape.as_slice())
    }
}

/// Writes a safetensors file (F32 or F16). For tests and tools.
pub fn to_safetensors(tensors: &[(&str, &[usize], &[f32])], f16: bool, metadata: &[(&str, &str)]) -> Vec<u8> {
    let mut header = serde_json::Map::new();
    let mut data = Vec::new();
    if !metadata.is_empty() {
        let m: serde_json::Map<String, serde_json::Value> = metadata.iter().map(|(k, v)| (k.to_string(), serde_json::Value::from(*v))).collect();
        header.insert("__metadata__".into(), serde_json::Value::Object(m));
    }
    for (name, shape, values) in tensors {
        let a = data.len();
        for v in *values {
            if f16 {
                data.extend_from_slice(&half::f16::from_f32(*v).to_le_bytes());
            } else {
                data.extend_from_slice(&v.to_le_bytes());
            }
        }
        header.insert(name.to_string(), serde_json::json!({ "dtype": if f16 { "F16" } else { "F32" }, "shape": shape, "data_offsets": [a, data.len()] }));
    }
    let h = serde_json::Value::Object(header).to_string().into_bytes();
    let mut out = (h.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(&h);
    out.extend_from_slice(&data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_f16_and_f32() {
        for f16 in [false, true] {
            let bytes = to_safetensors(&[("a.weight", &[2, 2], &[1.0, -2.0, 0.5, 3.0]), ("b", &[1], &[7.0])], f16, &[("arch", "esrgan")]);
            let w = Weights::from_safetensors(&bytes).unwrap();
            assert_eq!(w.get("a.weight", &[2, 2]).unwrap().data, vec![1.0, -2.0, 0.5, 3.0]);
            assert_eq!(w.metadata.get("arch").map(String::as_str), Some("esrgan"));
            assert!(w.get("a.weight", &[4]).is_err());
            assert!(w.get("missing", &[1]).is_err());
        }
    }

    #[test]
    fn hostile_files_are_errors() {
        assert!(Weights::from_safetensors(&[]).is_err());
        assert!(Weights::from_safetensors(&u64::MAX.to_le_bytes()).is_err());
        let mut b = to_safetensors(&[("a", &[4], &[1.0; 4])], false, &[]);
        b.truncate(b.len() - 1);
        assert!(Weights::from_safetensors(&b).is_err());
        let evil = br#"{"a":{"dtype":"F32","shape":[4611686018427387904,4],"data_offsets":[0,16]}}"#;
        let mut f = (evil.len() as u64).to_le_bytes().to_vec();
        f.extend_from_slice(evil);
        f.extend_from_slice(&[0; 16]);
        assert!(Weights::from_safetensors(&f).is_err());
        let evil = br#"{"a":{"dtype":"I8","shape":[1],"data_offsets":[1,0]}}"#;
        let mut f = (evil.len() as u64).to_le_bytes().to_vec();
        f.extend_from_slice(evil);
        assert!(Weights::from_safetensors(&f).is_err());
    }
}
