//! Workspace JSON boundary: Python integers retain integer spelling and range.
//! Use this codec rather than serde_json's unchecked text decoder for records.
use serde::de::IgnoredAny;
use serde_json::Value;
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

/// Decode a JSON value after checking every number, including unknown fields.
/// Integers stay i64/u64; schema conversion is a separate caller operation.
/// Floats remain f64; integer -0 becomes Python's integer zero.
pub fn from_str(text: &str) -> Result<Value, DecodeError> {
    let normalized = normalize(text)?;
    Ok(serde_json::from_str(&normalized)?)
}

/// Validate and normalize JSON text for callers that need a custom
/// deserializer while retaining the shared integer and negative-zero policy.
pub(crate) fn normalize(text: &str) -> Result<String, DecodeError> {
    // Validate the grammar before scanning, including enormous integer tokens.
    serde_json::from_str::<IgnoredAny>(text)?;
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        normalized.push(ch);
        if ch == '"' {
            while let Some((_, ch)) = chars.next() {
                normalized.push(ch);
                if ch == '\\' {
                    if let Some((_, escaped)) = chars.next() {
                        normalized.push(escaped);
                    }
                } else if ch == '"' {
                    break;
                }
            }
        } else if ch == '-' || ch.is_ascii_digit() {
            let mut end = start + ch.len_utf8();
            while let Some(&(index, next)) = chars.peek() {
                if !(next.is_ascii_digit() || matches!(next, '.' | 'e' | 'E' | '+' | '-')) {
                    break;
                }
                end = index + next.len_utf8();
                chars.next();
            }
            let token = &text[start..end];
            if !token.contains(['.', 'e', 'E'])
                && token.parse::<i64>().is_err()
                && token.parse::<u64>().is_err()
            {
                return Err(NumberOutOfRange {
                    value: token.into(),
                }
                .into());
            }
            normalized.pop();
            normalized.push_str(if token == "-0" { "0" } else { token });
        }
    }
    Ok(normalized)
}

pub fn from_slice(bytes: &[u8]) -> Result<Value, DecodeError> {
    // Preserve serde's typed syntax/encoding error for invalid UTF-8.
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return Err(serde_json::from_slice::<serde_json::Value>(bytes)
                .unwrap_err()
                .into())
        }
    };
    from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn integers_and_floats_keep_exact_spelling_and_range() {
        let raw = "[-9223372036854775808,9223372036854775807,18446744073709551615,9007199254740993,30,30.0,-0,-0.0]";
        let value: Value = from_str(raw).unwrap();
        assert_eq!(
            serde_json::to_string(&value).unwrap(),
            raw.replace(",-0,", ",0,")
        );
        assert_eq!(
            from_str("-9223372036854775808").unwrap().as_i64(),
            Some(i64::MIN)
        );
        assert_eq!(
            from_str("18446744073709551615").unwrap().as_u64(),
            Some(u64::MAX)
        );
    }

    #[test]
    fn out_of_range_integers_are_typed_even_in_unowned_fields() {
        for token in [
            "-9223372036854775809",
            "18446744073709551616",
            &"9".repeat(400),
        ] {
            let raw = format!("{{\"ignored\":[{token}]}}");
            let err = from_slice(raw.as_bytes()).err().unwrap();
            assert!(
                matches!(err, DecodeError::NumberOutOfRange(NumberOutOfRange { value }) if value == token)
            );
        }
    }

    #[test]
    fn strings_are_opaque_and_invalid_syntax_is_refused() {
        let raw = r#"{"18446744073709551616":"\"-0 18446744073709551616"}"#;
        let value: Value = from_str(raw).unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), raw);
        for raw in ["[01]", "[1e]", "[--1]", "NaN", "1e400"] {
            assert!(matches!(from_str(raw), Err(DecodeError::Json(_))));
        }
        for bytes in [&[255][..], &b"\"\xff\""[..]] {
            assert!(matches!(from_slice(bytes), Err(DecodeError::Json(_))));
        }
    }
}
