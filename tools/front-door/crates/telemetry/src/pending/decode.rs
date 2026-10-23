//! Checked JSON entry point until the records owner supplies its shared codec.
use crate::{Error, Result};
use serde::de::DeserializeOwned;

// port: belongs in provenant-records
/// Decode JSON without serde_json's lossy fallback for lexical integers outside i64/u64.
/// All external JSON bytes must cross this boundary before becoming a Value or record.
pub fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    let mut normalised = None;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    match bytes[index] {
                        b'\\' => index += 2,
                        b'"' => { index += 1; break; }
                        _ => index += 1,
                    }
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                index += 1;
                while index < bytes.len() && matches!(bytes[index], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                    index += 1;
                }
                let token = &bytes[start..index];
                if !token.iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
                    // Malformed tokens stay the JSON parser's responsibility.
                    let digits = token.strip_prefix(b"-").unwrap_or(token);
                    if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                        let text = std::str::from_utf8(token).map_err(|_| Error::NumberOutOfRange)?;
                        let fits = if token[0] == b'-' { text.parse::<i64>().is_ok() } else { text.parse::<u64>().is_ok() };
                        if !fits { return Err(Error::NumberOutOfRange); }
                        if token == b"-0" {
                            normalised.get_or_insert_with(|| bytes.to_vec())[start] = b' ';
                        }
                    }
                }
            }
            _ => index += 1,
        }
    }
    Ok(serde_json::from_slice(normalised.as_deref().unwrap_or(bytes))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
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
        for text in ["18446744073709551616", "-9223372036854775809", "{\"n\":100000000000000000001}"] {
            assert!(matches!(decode_json::<Value>(text.as_bytes()), Err(Error::NumberOutOfRange)));
        }
    }
}
