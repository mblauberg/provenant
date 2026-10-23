//! Workspace JSON boundary: Python integers retain integer spelling and range.
//! Use this codec before constructing values or typed records from JSON bytes.
use serde::de::{DeserializeOwned, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Number, Value};
use std::{
    borrow::Cow,
    fmt,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("JSON integer is outside i64/u64 range: {value}")]
pub struct NumberOutOfRange {
    pub value: String,
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    NumberOutOfRange(#[from] NumberOutOfRange),
}

#[derive(Debug, Error)]
pub enum ReadJsonError {
    #[error("cannot read JSON record at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON record at {path} exceeds the {limit}-byte limit")]
    TooLarge { path: PathBuf, limit: u64 },
    #[error("invalid JSON record at {path}")]
    Decode {
        path: PathBuf,
        #[source]
        source: DecodeError,
    },
}

/// Preflight all JSON integers, rejecting values outside Python's retained i64/u64
/// wire domain. Integer `-0` is normalized to Python's integer zero; decimal and
/// exponent forms retain float spelling. Callers may validate syntax separately.
pub fn check_integer_tokens(input: &[u8]) -> Result<Cow<'_, [u8]>, NumberOutOfRange> {
    let mut output: Option<Vec<u8>> = None;
    let mut copied_until = 0;
    let mut index = 0;
    let mut in_string = false;
    while index < input.len() {
        match input[index] {
            b'"' if !in_string => {
                in_string = true;
                index += 1;
            }
            b'"' if in_string => {
                in_string = false;
                index += 1;
            }
            b'\\' if in_string => {
                index = (index + 2).min(input.len());
            }
            b'-' | b'0'..=b'9' if !in_string => {
                let start = index;
                index += 1;
                while index < input.len()
                    && matches!(input[index], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    index += 1;
                }
                let token = &input[start..index];
                let digits = token.strip_prefix(b"-").unwrap_or(token);
                if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                    let Ok(text) = std::str::from_utf8(token) else {
                        return Err(NumberOutOfRange {
                            value: String::from_utf8_lossy(token).into_owned(),
                        });
                    };
                    let valid = if text.starts_with('-') {
                        text.parse::<i64>().is_ok()
                    } else {
                        text.parse::<u64>().is_ok()
                    };
                    if !valid {
                        return Err(NumberOutOfRange { value: text.into() });
                    }
                    if token == b"-0" {
                        let bytes = output.get_or_insert_with(Vec::new);
                        bytes.extend_from_slice(&input[copied_until..start]);
                        bytes.push(b'0');
                        copied_until = index;
                    }
                }
            }
            _ => index += 1,
        }
    }
    Ok(match output {
        Some(mut bytes) => {
            bytes.extend_from_slice(&input[copied_until..]);
            Cow::Owned(bytes)
        }
        None => Cow::Borrowed(input),
    })
}

/// Validate JSON grammar, integer range, and CPython's integer negative-zero rule.
pub(crate) fn normalize(text: &str) -> Result<String, DecodeError> {
    // Python rejects malformed JSON before any retained-integer range policy.
    // IgnoredAny validates grammar without rounding even enormous integer tokens.
    serde_json::from_str::<IgnoredAny>(text)?;
    let bytes = check_integer_tokens(text.as_bytes())?;
    String::from_utf8(bytes.into_owned()).map_err(|error| {
        DecodeError::Json(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error,
        )))
    })
}

/// Decode JSON to a dynamic value with lexical float parsing and checked integers.
pub fn from_str(text: &str) -> Result<Value, DecodeError> {
    from_slice(text.as_bytes())
}

/// Decode JSON bytes, preserving exact i64/u64 values and parsing float tokens from
/// their lexical spelling before serde's typed conversion.
pub fn from_slice(bytes: &[u8]) -> Result<Value, DecodeError> {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return Err(serde_json::from_slice::<Value>(bytes)
                .err()
                .unwrap_or_else(|| {
                    serde_json::Error::io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid UTF-8 JSON",
                    ))
                })
                .into());
        }
    };
    let normalized = normalize(text)?;
    let numbers = number_tokens(normalized.as_bytes())?;
    let mut parser = serde_json::Deserializer::from_slice(normalized.as_bytes());
    let value = CheckedValue(&mut numbers.into_iter()).deserialize(&mut parser)?;
    parser.end()?;
    Ok(value)
}

/// Decode a record after checking every numeric token, including unknown fields.
pub fn from_str_as<T: DeserializeOwned>(text: &str) -> Result<T, DecodeError> {
    from_slice_as(text.as_bytes())
}

/// Decode bytes to a typed record after checking every numeric token, including unknown fields.
pub fn from_slice_as<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, DecodeError> {
    Ok(serde_json::from_value(from_slice(bytes)?)?)
}

/// Python's schema check is equality with integer 1, so booleans and 1.0 also match.
pub fn schema_one(value: &Value) -> bool {
    let one = Value::Number(Number::from(1));
    provenant_core::py::python_json_eq(value, &one)
}

/// Read and decode one bounded JSON record with its path retained in typed failures.
pub fn read_json<T: DeserializeOwned>(path: &Path, limit: u64) -> Result<T, ReadJsonError> {
    let mut file = File::open(path).map_err(|source| ReadJsonError::Io {
        path: path.into(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| ReadJsonError::Io {
            path: path.into(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(ReadJsonError::TooLarge {
            path: path.into(),
            limit,
        });
    }
    from_slice_as(&bytes).map_err(|source| ReadJsonError::Decode {
        path: path.into(),
        source,
    })
}

pub(crate) fn number_tokens(input: &[u8]) -> Result<Vec<Number>, DecodeError> {
    let mut numbers = Vec::new();
    let mut index = 0;
    let mut quoted = false;
    while index < input.len() {
        match input[index] {
            b'"' => {
                quoted = !quoted;
                index += 1;
                if quoted {
                    while index < input.len() {
                        match input[index] {
                            b'\\' => index = (index + 2).min(input.len()),
                            b'"' => {
                                index += 1;
                                break;
                            }
                            _ => index += 1,
                        }
                    }
                    quoted = false;
                }
            }
            b'-' | b'0'..=b'9' if !quoted => {
                let start = index;
                index += 1;
                while index < input.len()
                    && matches!(input[index], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    index += 1;
                }
                let token =
                    std::str::from_utf8(&input[start..index]).map_err(|_| invalid_json(input))?;
                if token.contains(['.', 'e', 'E']) {
                    let float = token.parse::<f64>().map_err(|_| invalid_json(input))?;
                    numbers.push(Number::from_f64(float).ok_or_else(|| invalid_json(input))?);
                } else if token.starts_with('-') {
                    numbers.push(Number::from(token.parse::<i64>().map_err(|_| {
                        NumberOutOfRange {
                            value: token.into(),
                        }
                    })?));
                } else {
                    numbers.push(Number::from(token.parse::<u64>().map_err(|_| {
                        NumberOutOfRange {
                            value: token.into(),
                        }
                    })?));
                }
            }
            _ => index += 1,
        }
    }
    Ok(numbers)
}

fn invalid_json(input: &[u8]) -> serde_json::Error {
    serde_json::from_slice::<Value>(input)
        .err()
        .unwrap_or_else(|| {
            serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid JSON number",
            ))
        })
}

struct CheckedValue<'a>(&'a mut std::vec::IntoIter<Number>);
impl CheckedValue<'_> {
    fn number<E: serde::de::Error>(self) -> Result<Value, E> {
        self.0
            .next()
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }
}
impl<'de> DeserializeSeed<'de> for CheckedValue<'_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for CheckedValue<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.into()))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Value, E> {
        self.number()
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Value, E> {
        self.number()
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Value, E> {
        self.number()
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(CheckedValue(self.0))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            values.insert(key, map.next_value_seed(CheckedValue(self.0))?);
        }
        Ok(Value::Object(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[test]
    fn checked_values_cover_integer_boundaries_float_lexemes_and_escapes() {
        let raw = br#"[-9223372036854775808,18446744073709551615,9007199254740993,30,30.0,-0,-0.0,0.09090909090909091,1e20,"18446744073709551616","\\\"18446744073709551617"]"#;
        let value = from_slice(raw).unwrap();
        assert_eq!(value[0].as_i64(), Some(i64::MIN));
        assert_eq!(value[1].as_u64(), Some(u64::MAX));
        assert_eq!(value[2].as_u64(), Some(9_007_199_254_740_993));
        assert_eq!(value[3].to_string(), "30");
        assert_eq!(value[4].to_string(), "30.0");
        assert_eq!(value[5].to_string(), "0");
        assert_eq!(value[6].to_string(), "-0.0");
        assert_eq!(value[7].to_string(), "0.09090909090909091");
        assert_eq!(value[8].to_string(), "1e+20");
        assert!(value[9].is_string() && value[10].is_string());
    }

    #[test]
    fn range_errors_include_unknown_fields_but_ignore_quoted_digits() {
        for token in [
            "-9223372036854775809",
            "18446744073709551616",
            &"9".repeat(400),
        ] {
            let raw = format!(r#"{{"unknown":[{token}]}}"#);
            let err = from_str(&raw).unwrap_err();
            assert!(
                matches!(err, DecodeError::NumberOutOfRange(NumberOutOfRange { value }) if value == token)
            );
        }
        let raw = r#"{"18446744073709551616":"\\\"-0 18446744073709551616"}"#;
        assert!(from_str(raw).is_ok());
    }

    #[test]
    fn malformed_json_and_invalid_utf8_remain_typed_syntax_errors() {
        for text in ["[01]", "[1e]", "[--1]", "NaN", "1e400", "{\"x\":"] {
            assert!(matches!(from_str(text), Err(DecodeError::Json(_))));
        }
        for bytes in [&[255][..], &b"\"\xff\""[..]] {
            assert!(matches!(from_slice(bytes), Err(DecodeError::Json(_))));
        }
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct Record {
        n: u64,
    }
    #[test]
    fn typed_decode_checks_unknown_values_before_schema_conversion() {
        assert_eq!(
            from_str_as::<Record>(r#"{"n":18446744073709551615,"extra":1.0}"#).unwrap(),
            Record { n: u64::MAX }
        );
        assert!(matches!(
            from_str_as::<Record>(r#"{"n":1,"extra":18446744073709551616}"#),
            Err(DecodeError::NumberOutOfRange(_))
        ));
    }

    #[test]
    fn python_schema_one_uses_cross_numeric_equality() {
        for value in [json!(true), json!(1), json!(1.0)] {
            assert!(schema_one(&value));
        }
        for value in [json!(false), json!(0), json!("1"), json!(2.0)] {
            assert!(!schema_one(&value));
        }
    }

    #[test]
    fn checked_decode_matches_python_numeric_and_duplicate_key_semantics() {
        use std::{io::Write, process::Stdio};
        let fixture = br#"{"z":0.09090909090909091,"a":[-9223372036854775808,18446744073709551615,9007199254740993,30.0,-0,-0.0,1e20],"duplicate":1,"duplicate":2}"#;
        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        command
            .args([
                "-c",
                "import json,struct,sys; f=lambda x: ['float64',struct.pack('>d',x).hex()] if isinstance(x,float) else ([f(v) for v in x] if isinstance(x,list) else ({k:f(v) for k,v in x.items()} if isinstance(x,dict) else x)); print(json.dumps(f(json.load(sys.stdin)),sort_keys=True))",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            crate::oracle::spawn(&mut command).expect("configured Python oracle failed to start");
        if let Err(error) = child
            .child_mut()
            .stdin
            .take()
            .expect("Python oracle stdin was not piped")
            .write_all(fixture)
        {
            let _ = child.wait();
            panic!("could not send checked JSON fixture to Python oracle: {error}");
        }
        let output = child
            .wait()
            .expect("Python oracle failed or timed out after interpreter gate");
        assert!(
            output.status.success(),
            "Python oracle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        fn float_bits(value: Value) -> Value {
            match value {
                Value::Number(number) if number.is_f64() => Value::Array(vec![
                    Value::String("float64".into()),
                    Value::String(format!("{:016x}", number.as_f64().unwrap().to_bits())),
                ]),
                Value::Array(items) => Value::Array(items.into_iter().map(float_bits).collect()),
                Value::Object(items) => Value::Object(
                    items
                        .into_iter()
                        .map(|(key, value)| (key, float_bits(value)))
                        .collect(),
                ),
                value => value,
            }
        }
        let expected: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            float_bits(from_slice(fixture).unwrap()),
            expected,
            "Python decoder differential"
        );
    }

    #[test]
    fn bounded_read_reports_size_and_io_failures() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("record.json");
        std::fs::write(&path, br#"{"n":4}"#).unwrap();
        assert_eq!(read_json::<Value>(&path, 16).unwrap()["n"], 4);
        assert!(matches!(
            read_json::<Value>(&path, 3),
            Err(ReadJsonError::TooLarge { limit: 3, .. })
        ));
        assert!(matches!(
            read_json::<Value>(&temp.path().join("missing"), 10),
            Err(ReadJsonError::Io { .. })
        ));
    }
}

#[cfg(test)]
mod syntax_precedence {
    use super::*;
    #[test]
    fn malformed_tokens_report_syntax_before_range() {
        for raw in [
            "+18446744073709551616",
            "[018446744073709551616]",
            "{18446744073709551616:0}",
        ] {
            assert!(matches!(from_str(raw), Err(DecodeError::Json(_))), "{raw}");
        }
    }
}
