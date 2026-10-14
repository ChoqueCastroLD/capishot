//! Library ids and JSON number helpers.
//!
//! TypeR ids are strings (random base-36) or, in old libraries, finite numbers. Both are kept as
//! they were so an imported library re-exports unchanged.

use serde_json::{Number, Value};

/// A style or folder id: a non-empty string shorter than 200 characters, or a finite number
/// (TypeR's `validId`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Id {
    Str(String),
    Num(Number),
}

impl Id {
    /// The id in `v`, if it is a valid TypeR id.
    pub fn from_value(v: &Value) -> Option<Id> {
        match v {
            Value::String(s) if !s.is_empty() && s.chars().count() < 200 => Some(Id::Str(s.clone())),
            Value::Number(n) if n.as_f64().is_some_and(f64::is_finite) => Some(Id::Num(n.clone())),
            _ => None,
        }
    }

    /// A parent/folder reference: `null`, `""`, a missing key or anything invalid is "none"
    /// (TypeR treats every falsy value as the root / Unsorted).
    pub fn opt_from_value(v: Option<&Value>) -> Option<Id> {
        v.and_then(Id::from_value)
    }

    pub fn to_value(&self) -> Value {
        match self {
            Id::Str(s) => Value::String(s.clone()),
            Id::Num(n) => Value::Number(n.clone()),
        }
    }

    pub fn str(s: &str) -> Id {
        Id::Str(s.to_string())
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Id::Str(s) => f.write_str(s),
            Id::Num(n) => write!(f, "{n}"),
        }
    }
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Id::str(s)
    }
}

impl serde::Serialize for Id {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for Id {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Id::from_value(&v).ok_or_else(|| serde::de::Error::custom("an id is a non-empty string or a finite number"))
    }
}

/// A JSON number for `v`, written as an integer when it is whole (TypeR writes `23`, not `23.0`).
/// Non-finite values become `null`.
pub fn num(v: f64) -> Value {
    if !v.is_finite() {
        return Value::Null;
    }
    if v.fract() == 0.0 && v.abs() < 9.0e15 {
        // Whole and within the exactly representable integer range.
        return Value::Number(Number::from(v as i64));
    }
    Number::from_f64(v).map_or(Value::Null, Value::Number)
}

/// JavaScript's `parseFloat` for the values TypeR stores: numbers as is, strings by their leading
/// number; anything else is `None`. Non-finite results are `None`.
pub fn parse_float(v: Option<&Value>) -> Option<f64> {
    let f = match v? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => leading_number(s.trim_start())?,
        _ => return None,
    };
    f.is_finite().then_some(f)
}

/// The longest prefix of `s` that parses as a decimal number (`"12px"` → 12).
fn leading_number(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut end = 0;
    let mut seen_digit = false;
    let mut seen_dot = false;
    let mut seen_exp = false;
    let mut i = 0;
    if matches!(bytes.first(), Some(b'+' | b'-')) {
        i = 1;
    }
    while let Some(&b) = bytes.get(i) {
        match b {
            b'0'..=b'9' => {
                seen_digit = true;
                end = i + 1;
            }
            b'.' if !seen_dot && !seen_exp => seen_dot = true,
            b'e' | b'E' if seen_digit && !seen_exp => {
                seen_exp = true;
                if matches!(bytes.get(i + 1), Some(b'+' | b'-')) {
                    i += 1;
                }
            }
            _ => break,
        }
        i += 1;
    }
    if !seen_digit {
        return None;
    }
    s.get(..end)?.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_follow_typer_valid_id() {
        assert_eq!(Id::from_value(&json!("a")), Some(Id::str("a")));
        assert_eq!(Id::from_value(&json!("")), None);
        assert_eq!(Id::from_value(&json!("x".repeat(200))), None);
        assert!(Id::from_value(&json!(3)).is_some());
        assert_eq!(Id::from_value(&json!(null)), None);
        assert_eq!(Id::from_value(&json!([1])), None);
    }

    #[test]
    fn numbers_stay_integers_when_whole() {
        assert_eq!(num(23.0).to_string(), "23");
        assert_eq!(num(10.5).to_string(), "10.5");
        assert_eq!(num(f64::NAN), Value::Null);
    }

    #[test]
    fn parse_float_reads_leading_numbers() {
        assert_eq!(parse_float(Some(&json!("12px"))), Some(12.0));
        assert_eq!(parse_float(Some(&json!(" 3.5"))), Some(3.5));
        assert_eq!(parse_float(Some(&json!("bad"))), None);
        assert_eq!(parse_float(Some(&json!(true))), None);
        assert_eq!(parse_float(Some(&json!("1e3"))), Some(1000.0));
        assert_eq!(parse_float(None), None);
    }
}
