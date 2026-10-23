//! Checked JSON entry point until the records owner supplies its shared codec.
use crate::{Error, Result};
use serde::de::{DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Number, Value};

// port: belongs in provenant-records
/// Decode JSON without serde_json's lossy fallback for lexical integers outside i64/u64.
/// All external JSON bytes must cross this boundary before becoming a Value or record.
pub fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.len() > crate::MAX_JSON_BYTES {
        return Err(Error::InputTooLarge {
            limit: crate::MAX_JSON_BYTES,
        });
    }
    let mut numbers = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'"' => {
                            index += 1;
                            break;
                        }
                        _ => index += 1,
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && matches!(bytes[index], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    index += 1;
                }
                let token = &bytes[start..index];
                if let Ok(text) = std::str::from_utf8(token) {
                    if token.iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
                        if let Some(number) = text.parse::<f64>().ok().and_then(Number::from_f64) {
                            numbers.push(number);
                        }
                    } else {
                        let digits = token.strip_prefix(b"-").unwrap_or(token);
                        if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                            let number = if token[0] == b'-' {
                                text.parse::<i64>()
                                    .map(Number::from)
                                    .map_err(|_| Error::NumberOutOfRange)?
                            } else {
                                text.parse::<u64>()
                                    .map(Number::from)
                                    .map_err(|_| Error::NumberOutOfRange)?
                            };
                            numbers.push(number);
                        }
                    }
                }
            }
            _ => index += 1,
        }
    }
    // serde_json's default float decoder can differ by an ULP from CPython.
    // Keep its syntax/depth checks, supplying numbers parsed from their lexical tokens.
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    let value = CheckedValue(&mut numbers.into_iter()).deserialize(&mut parser)?;
    parser.end()?;
    Ok(serde_json::from_value(value)?)
}

// port: belongs in provenant-records
struct CheckedValue<'a>(&'a mut std::vec::IntoIter<Number>);
// port: belongs in provenant-records
impl CheckedValue<'_> {
    fn number<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
        self.0
            .next()
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }
}
// port: belongs in provenant-records
impl<'de> DeserializeSeed<'de> for CheckedValue<'_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}
// port: belongs in provenant-records
impl<'de> Visitor<'de> for CheckedValue<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(v.into()))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> std::result::Result<Value, E> {
        Ok(Value::String(v))
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Value, E> {
        self.number()
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> std::result::Result<Value, E> {
        self.number()
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Value, E> {
        self.number()
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(CheckedValue(self.0))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
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

    #[test]
    fn checked_decode_retains_spelling_at_integer_boundaries() {
        let text = br#"[-9223372036854775808,18446744073709551615,9007199254740993,0,0.0,-0.0,1e30,-0,"18446744073709551616",{"escaped":"\\\"18446744073709551616"}]"#;
        let value: Value = decode_json(text).unwrap();
        assert_eq!(value[0].to_string(), "-9223372036854775808");
        assert_eq!(value[1].to_string(), "18446744073709551615");
        assert_eq!(value[2].to_string(), "9007199254740993");
        assert_eq!(value[3].to_string(), "0");
        assert_eq!(value[4].to_string(), "0.0");
        assert_eq!(value[5].to_string(), "-0.0");
        assert_eq!(value[7].to_string(), "0");
        let value: Value = decode_json(
            br#"{"z":0.09090909090909091,"a":[0.1,0.0,0],"duplicate":1,"duplicate":2}"#,
        )
        .unwrap();
        assert_eq!(value["z"].to_string(), "0.09090909090909091");
        assert_eq!(value["a"][1].to_string(), "0.0");
        assert_eq!(value["a"][2].to_string(), "0");
        assert_eq!(value["duplicate"].to_string(), "2");
        for text in [
            "18446744073709551616",
            "-9223372036854775809",
            "{\"n\":100000000000000000001}",
        ] {
            assert!(matches!(
                decode_json::<Value>(text.as_bytes()),
                Err(Error::NumberOutOfRange)
            ));
        }
    }
}
