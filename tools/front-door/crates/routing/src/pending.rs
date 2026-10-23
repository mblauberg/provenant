//! Compatibility helpers awaiting their owning crates.

// port: belongs in provenant-core
pub(crate) fn python_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}'))
}

// port: belongs in provenant-core
pub(crate) fn truthy(value: &crate::ordered_json::OrderedValue) -> bool {
    use crate::ordered_json::OrderedValue;
    match value {
        OrderedValue::Null => false,
        OrderedValue::Bool(value) => *value,
        OrderedValue::Number(value) => {
            if value.is_f64() {
                value.as_f64().is_some_and(|number| number != 0.0)
            } else {
                value.as_i64() != Some(0) && value.as_u64() != Some(0)
            }
        }
        OrderedValue::String(value) => !value.is_empty(),
        OrderedValue::Array(value) => !value.is_empty(),
        OrderedValue::Object(value) => !value.0.is_empty(),
    }
}

// port: belongs in provenant-records
#[derive(Debug, thiserror::Error)]
pub enum JsonError {
    #[error("JSON integer is outside the i64/u64 range")]
    NumberOutOfRange,
    #[error(transparent)]
    Invalid(#[from] serde_json::Error),
}

/// Checked JSON ingress until the records owner supplies the shared codec.
/// Validate integer tokens before serde_json can silently coerce them to floats.
// port: belongs in provenant-records
pub fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, JsonError> {
    let bytes = text.as_bytes();
    let mut index = 0;
    let mut quoted = false;
    let mut normalized: Option<String> = None;
    let mut copied_until = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        } else if byte == b'-' || byte.is_ascii_digit() {
            let start = index;
            index += 1;
            while index < bytes.len()
                && matches!(bytes[index], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
            {
                index += 1;
            }
            let token = &text[start..index];
            // CPython json.loads("-0") returns integer 0. serde_json otherwise
            // treats that integer token as float -0.0; float tokens keep their sign.
            if token == "-0" {
                let output = normalized.get_or_insert_with(|| String::with_capacity(text.len()));
                output.push_str(&text[copied_until..start]);
                output.push('0');
                copied_until = index;
            }
            let digits = token.strip_prefix('-').unwrap_or(token);
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                let valid = if token.starts_with('-') {
                    token.parse::<i64>().is_ok()
                } else {
                    token.parse::<u64>().is_ok()
                };
                if !valid {
                    return Err(JsonError::NumberOutOfRange);
                }
            }
            continue;
        }
        index += 1;
    }
    if let Some(mut output) = normalized {
        output.push_str(&text[copied_until..]);
        Ok(serde_json::from_str(&output)?)
    } else {
        Ok(serde_json::from_str(text)?)
    }
}

use serde_json::Value;

// port: belongs in provenant-core
pub(crate) fn python_string(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(number) => {
            let rendered = number.to_string();
            if let Some(value) = number
                .is_f64()
                .then(|| number.as_f64())
                .flatten()
                .filter(|value| *value != 0.0 && (value.abs() < 1e-4 || value.abs() >= 1e16))
            {
                let shortest = format!("{value:?}");
                if let Some((mantissa, exponent)) = shortest.split_once('e') {
                    if let Ok(exponent) = exponent.parse::<i32>() {
                        return format!(
                            "{mantissa}e{}{:02}",
                            if exponent < 0 { '-' } else { '+' },
                            exponent.unsigned_abs()
                        );
                    }
                }
            }
            rendered
        }
        Value::String(text) => text.clone(),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(python_value_repr)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(items) => format!(
            "{{{}}}",
            items
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    python_value_repr(&Value::String(key.clone())),
                    python_value_repr(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

// port: belongs in provenant-core
pub(crate) fn python_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    for c in text.chars() {
        match c {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if c == quote => {
                result.push('\\');
                result.push(c);
            }
            c if !crate::casefold::is_printable(c) => {
                let code = u32::from(c);
                if code < 256 {
                    result.push_str(&format!("\\x{code:02x}"));
                } else if code <= 0xffff {
                    result.push_str(&format!("\\u{code:04x}"));
                } else {
                    result.push_str(&format!("\\U{code:08x}"));
                }
            }
            c => result.push(c),
        }
    }
    result.push(quote);
    result
}

// port: belongs in provenant-core
fn python_value_repr(value: &Value) -> String {
    match value {
        Value::String(text) => python_repr(text),
        _ => python_string(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ordered_json::OrderedValue;

    #[test]
    fn integer_negative_zero_stays_an_integer_and_float_negative_zero_stays_a_float() {
        for (wire, expected) in [
            ("-0", "0"),
            ("[-0,-0.0]", "[0,-0.0]"),
            (r#"{"x":-0,"text":"-0"}"#, r#"{"x":0,"text":"-0"}"#),
        ] {
            let value: OrderedValue = from_json(wire).unwrap();
            assert_eq!(serde_json::to_string(&value).unwrap(), expected);
        }
    }

    #[test]
    fn integer_ingress_preserves_limits_and_refuses_overflow() {
        for token in [
            "-9223372036854775808",
            "9223372036854775807",
            "18446744073709551615",
            "9007199254740993",
        ] {
            let parsed: OrderedValue = from_json(token).unwrap();
            assert_eq!(serde_json::to_string(&parsed).unwrap(), token);
        }
        for token in ["-9223372036854775809", "18446744073709551616"] {
            assert!(matches!(
                from_json::<OrderedValue>(token),
                Err(JsonError::NumberOutOfRange)
            ));
        }
        let text = r#"{"integer-looking text":"18446744073709551616","float":1.0}"#;
        assert_eq!(
            serde_json::to_string(&from_json::<OrderedValue>(text).unwrap()).unwrap(),
            text
        );
    }
}
