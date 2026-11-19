//! SQLite values in JSON (docs/api.md, "Value encoding").
//!
//! | SQLite | JSON |
//! | --- | --- |
//! | NULL | `null` |
//! | INTEGER, \|n\| ≤ 2^53 − 1 | number without fraction or exponent |
//! | INTEGER beyond that | `{"type":"integer","value":"<decimal>"}` |
//! | REAL, finite | number, always with a fraction or exponent (`1.0`) |
//! | REAL, ±infinity | `{"type":"real","value":"Infinity"}` or `"-Infinity"` |
//! | TEXT | string |
//! | BLOB | `{"type":"blob","value":"<standard base64, padded>"}` |
//!
//! Parameters accept every output form, plus `true`/`false` (INTEGER 1/0),
//! JSON integers of any size within i64, and the tagged forms
//! `{"type":"integer","value":<number or string>}`,
//! `{"type":"real","value":<number or string>}` and
//! `{"type":"text","value":"…"}`.
//!
//! A bare number's kind comes from its JSON token: with a fraction or
//! exponent it is REAL, without one an INTEGER that must fit i64. So
//! [`SqlValue`] (and [`crate::Params`]) deserialize from JSON text
//! (`serde_json::from_str`, `from_slice`, axum's `Json`), not from an
//! already parsed `serde_json::Value`, which has lost the token.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Map, Value, json, value::RawValue};

/// Largest integer a JSON number carries exactly in an IEEE 754 double.
pub const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// One SQLite value.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl SqlValue {
    /// The JSON form of the value.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Integer(n) if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(n) => json!(n),
            Self::Integer(n) => json!({"type": "integer", "value": n.to_string()}),
            Self::Real(x) if x.is_finite() => json!(x),
            Self::Real(x) if x.is_nan() => Value::Null,
            Self::Real(x) => json!({
                "type": "real",
                "value": if *x > 0.0 { "Infinity" } else { "-Infinity" },
            }),
            Self::Text(s) => Value::String(s.clone()),
            Self::Blob(b) => json!({"type": "blob", "value": STANDARD.encode(b)}),
        }
    }

    /// Reads a value in any accepted parameter form from its JSON text.
    ///
    /// # Errors
    ///
    /// As [`SqlValue::from_json`], and an integer token beyond i64 or a
    /// real token beyond f64 on either side.
    pub fn from_json_text(text: &str) -> Result<Self, &'static str> {
        let text = text.trim();
        // Parsing first rejects what JSON does not allow (`01`, `1.`, `+1`).
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) if valid_number_token(text) => return number_token(text),
            Err(_) => return Err("invalid JSON"),
        };
        if value.is_number() {
            return number_token(text);
        }
        Self::from_json(&value)
    }

    /// Reads a value in any accepted parameter form. Prefer
    /// [`SqlValue::from_json_text`]: a parsed `Value` holds an integer
    /// beyond i64 as a float, which this reads as REAL.
    ///
    /// # Errors
    ///
    /// A short reason when the JSON is not a value: an array, an object
    /// that is not a tagged value, a number beyond i64 or NaN.
    pub fn from_json(value: &Value) -> Result<Self, &'static str> {
        match value {
            Value::Null => Ok(Self::Null),
            Value::Bool(b) => Ok(Self::Integer(i64::from(*b))),
            Value::Number(n) => number(n),
            Value::String(s) => Ok(Self::Text(s.clone())),
            Value::Array(_) => Err("arrays are not SQLite values"),
            Value::Object(object) => tagged(object),
        }
    }
}

/// Whether `token` has JSON number syntax:
/// `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`. serde_json refuses a
/// syntactically valid number only when it overflows f64 (`1e400`).
fn valid_number_token(token: &str) -> bool {
    let b = token.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let digits = |i: &mut usize| {
        let start = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i > start
    };
    match b.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            digits(&mut i);
        }
        _ => return false,
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == b.len()
}

/// A bare JSON number token: REAL with a fraction or exponent, otherwise
/// an INTEGER within i64.
fn number_token(token: &str) -> Result<SqlValue, &'static str> {
    if token.contains(['.', 'e', 'E']) {
        token
            .parse::<f64>()
            .ok()
            .filter(|x| x.is_finite())
            .map(SqlValue::Real)
            .ok_or("real out of range")
    } else {
        token
            .parse::<i64>()
            .map(SqlValue::Integer)
            .map_err(|_| "integer out of range")
    }
}

fn number(n: &serde_json::Number) -> Result<SqlValue, &'static str> {
    if let Some(i) = n.as_i64() {
        Ok(SqlValue::Integer(i))
    } else if n.is_u64() {
        Err("integer out of range")
    } else {
        n.as_f64()
            .map(SqlValue::Real)
            .ok_or("number not representable")
    }
}

fn tagged(object: &Map<String, Value>) -> Result<SqlValue, &'static str> {
    if object.len() != 2 {
        return Err("a tagged value has exactly `type` and `value`");
    }
    let (Some(Value::String(kind)), Some(value)) = (object.get("type"), object.get("value")) else {
        return Err("a tagged value has exactly `type` and `value`");
    };
    match (kind.as_str(), value) {
        ("integer", Value::Number(n)) => match number(n)? {
            integer @ SqlValue::Integer(_) => Ok(integer),
            _ => Err("integer has a fraction"),
        },
        ("integer", Value::String(s)) => s
            .parse::<i64>()
            .map(SqlValue::Integer)
            .map_err(|_| "integer is not a decimal i64"),
        ("real", Value::Number(n)) => n.as_f64().map(SqlValue::Real).ok_or("invalid real"),
        ("real", Value::String(s)) => match s.as_str() {
            "Infinity" => Ok(SqlValue::Real(f64::INFINITY)),
            "-Infinity" => Ok(SqlValue::Real(f64::NEG_INFINITY)),
            _ => s
                .parse::<f64>()
                .ok()
                .filter(|x| x.is_finite())
                .map(SqlValue::Real)
                .ok_or("invalid real"),
        },
        ("text", Value::String(s)) => Ok(SqlValue::Text(s.clone())),
        ("blob", Value::String(s)) => STANDARD
            .decode(s)
            .map(SqlValue::Blob)
            .map_err(|_| "blob is not padded standard base64"),
        _ => Err("unknown tagged value"),
    }
}

impl Serialize for SqlValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_json().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SqlValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        Self::from_json_text(raw.get()).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(v: SqlValue) -> String {
        serde_json::to_string(&v).unwrap()
    }
    fn parse(text: &str) -> Result<SqlValue, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    #[test]
    fn integers_switch_to_strings_past_two_to_the_53() {
        assert_eq!(out(SqlValue::Integer(MAX_SAFE_INTEGER)), "9007199254740991");
        assert_eq!(
            out(SqlValue::Integer(-MAX_SAFE_INTEGER)),
            "-9007199254740991"
        );
        assert_eq!(
            out(SqlValue::Integer(MAX_SAFE_INTEGER + 1)),
            r#"{"type":"integer","value":"9007199254740992"}"#
        );
        assert_eq!(
            out(SqlValue::Integer(i64::MIN)),
            r#"{"type":"integer","value":"-9223372036854775808"}"#
        );
    }

    #[test]
    fn reals_stay_numbers_with_a_fraction() {
        assert_eq!(out(SqlValue::Real(1.0)), "1.0");
        assert_eq!(out(SqlValue::Real(0.5)), "0.5");
        assert_eq!(
            out(SqlValue::Real(f64::NEG_INFINITY)),
            r#"{"type":"real","value":"-Infinity"}"#
        );
        assert_eq!(parse("1.0"), Ok(SqlValue::Real(1.0)));
        assert_eq!(parse("1e3"), Ok(SqlValue::Real(1000.0)));
        assert_eq!(parse("1"), Ok(SqlValue::Integer(1)));
        assert_eq!(parse("-0"), Ok(SqlValue::Integer(0)));
        assert_eq!(parse("1E2"), Ok(SqlValue::Real(100.0)));
    }

    #[test]
    fn blobs_are_tagged_base64() {
        assert_eq!(
            out(SqlValue::Blob(vec![0, 255, 16])),
            r#"{"type":"blob","value":"AP8Q"}"#
        );
        assert_eq!(
            parse(r#"{"type":"blob","value":"AP8Q"}"#),
            Ok(SqlValue::Blob(vec![0, 255, 16]))
        );
        assert!(parse(r#"{"type":"blob","value":"AP8Q!"}"#).is_err());
    }

    #[test]
    fn every_output_form_reads_back() {
        for v in [
            SqlValue::Null,
            SqlValue::Integer(42),
            SqlValue::Integer(i64::MAX),
            SqlValue::Integer(i64::MIN),
            SqlValue::Real(-2.25),
            SqlValue::Real(f64::INFINITY),
            SqlValue::Text("héllo".into()),
            SqlValue::Text(String::new()),
            SqlValue::Blob(Vec::new()),
            SqlValue::Blob(b"\x00bytes".to_vec()),
        ] {
            assert_eq!(parse(&out(v.clone())), Ok(v));
        }
    }

    #[test]
    fn parameters_accept_the_extra_forms() {
        assert_eq!(parse("true"), Ok(SqlValue::Integer(1)));
        assert_eq!(parse("false"), Ok(SqlValue::Integer(0)));
        assert_eq!(
            parse("9223372036854775807"),
            Ok(SqlValue::Integer(i64::MAX))
        );
        assert_eq!(
            parse("-9223372036854775808"),
            Ok(SqlValue::Integer(i64::MIN))
        );
        assert_eq!(
            parse(r#"{"type":"integer","value":12}"#),
            Ok(SqlValue::Integer(12))
        );
        assert_eq!(
            parse(r#"{"type":"real","value":"2.5"}"#),
            Ok(SqlValue::Real(2.5))
        );
        assert_eq!(
            parse(r#"{"type":"text","value":"{}"}"#),
            Ok(SqlValue::Text("{}".into()))
        );
    }

    #[test]
    fn parameters_reject_what_sqlite_cannot_hold() {
        for bad in [
            "[1]",
            "9223372036854775808",
            "-9223372036854775809",
            "1e400",
            "-1e400",
            r#"{"value":"1"}"#,
            r#"{"type":"integer","value":"1.5"}"#,
            r#"{"type":"integer","value":1.5}"#,
            r#"{"type":"real","value":"NaN"}"#,
            r#"{"type":"date","value":"2026"}"#,
            r#"{"type":"text","value":"a","extra":1}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn from_json_text_refuses_non_json_numbers() {
        for bad in ["01", "1.", "+1", ".5", "1e", "0x10", "--1"] {
            assert!(SqlValue::from_json_text(bad).is_err(), "{bad}");
        }
        assert_eq!(SqlValue::from_json_text("-0.5e1"), Ok(SqlValue::Real(-5.0)));
        assert!(SqlValue::from_json_text("1e400").is_err());
    }

    #[test]
    fn nan_reads_as_null_like_sqlite_stores_it() {
        assert_eq!(SqlValue::Real(f64::NAN).to_json(), Value::Null);
    }
}
