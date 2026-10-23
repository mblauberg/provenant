//! Private compatibility primitives awaiting their owning crate.
use serde_json::Value;

// port: belongs in provenant-core
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IntegerError {
    #[error("integer is outside the i64/u64 range")]
    NumberOutOfRange,
    #[error("invalid Python int input")]
    InvalidInteger,
}

// port: belongs in provenant-core
/// Python int() for JSON inputs; retain signed integer values before clamping.
fn python_integer(value: &Value) -> Result<i128, IntegerError> {
    let integer: i128 = match value {
        Value::Bool(value) => i128::from(*value),
        Value::Number(value) if value.is_i64() => {
            i128::from(value.as_i64().ok_or(IntegerError::InvalidInteger)?)
        }
        Value::Number(value) if value.is_u64() => {
            i128::from(value.as_u64().ok_or(IntegerError::InvalidInteger)?)
        }
        Value::Number(value) => {
            let value = value.as_f64().ok_or(IntegerError::InvalidInteger)?.trunc();
            if !(-9_223_372_036_854_775_808.0..18_446_744_073_709_551_616.0).contains(&value) {
                return Err(IntegerError::NumberOutOfRange);
            }
            value as i128
        }
        Value::String(value) => {
            let value = value.trim();
            let (sign, digits) = if let Some(v) = value.strip_prefix('-') {
                (-1, v)
            } else {
                (1, value.strip_prefix('+').unwrap_or(value))
            };
            let mut normalized = String::new();
            let mut previous_digit = false;
            for ch in digits.chars() {
                if ch == '_' && previous_digit {
                    previous_digit = false;
                    continue;
                }
                let digit = decimal_digit(ch).ok_or(IntegerError::InvalidInteger)?;
                normalized.push(char::from(b'0' + digit));
                previous_digit = true;
            }
            if !previous_digit {
                return Err(IntegerError::InvalidInteger);
            }
            normalized
                .parse::<i128>()
                .map_err(|_| IntegerError::NumberOutOfRange)?
                * sign
        }
        _ => return Err(IntegerError::InvalidInteger),
    };
    if integer < i128::from(i64::MIN) || integer > i128::from(u64::MAX) {
        return Err(IntegerError::NumberOutOfRange);
    }
    Ok(integer)
}

// port: belongs in provenant-core
/// Unicode decimal blocks (Nd); unlike char::to_digit this includes Python's digits.
pub(crate) fn decimal_digit(ch: char) -> Option<u8> {
    if ch.is_ascii_digit() {
        return Some(ch as u8 - b'0');
    }
    // The Unicode regex crate owns the category table; contiguous decimal runs
    // comprise one or more ordered sets of ten digits.
    let re = regex::Regex::new(r"^\p{Nd}$").ok()?;
    if !re.is_match(ch.encode_utf8(&mut [0; 4])) {
        return None;
    }
    let scalar = u32::from(ch);
    let mut start = scalar;
    while start > 0
        && char::from_u32(start - 1).is_some_and(|c| re.is_match(c.encode_utf8(&mut [0; 4])))
    {
        start -= 1;
    }
    Some(((scalar - start) % 10) as u8)
}

// port: belongs in provenant-core
// Core owns the matching Unicode table privately; export an isprintable API
// before cut-over and delete this category predicate.
pub(crate) fn python_isprintable(value: &str) -> bool {
    let Ok(non_printable) = regex::Regex::new(r"[\p{C}\p{Z}]") else {
        return false;
    };
    value
        .chars()
        .all(|c| c == ' ' || !non_printable.is_match(c.encode_utf8(&mut [0; 4])))
}

// port: belongs in provenant-core
pub fn positive_timeout(value: &Value) -> Result<u64, IntegerError> {
    Ok(python_integer(value)?.max(1) as u64)
}
// port: belongs in provenant-core
pub(crate) fn integer_string(value: &Value) -> Result<String, IntegerError> {
    Ok(python_integer(value)?.to_string())
}

// port: belongs in provenant-records
// checked_json::normalize is private on main. Validate with its public codec,
// then apply only its -0 token normalization to retain object order while
// deserializing typed maps. Delete when records exposes checked deserialization.
pub(crate) fn normalize_negative_zero(input: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'"' {
            result.push(input[i]);
            i += 1;
            while i < input.len() {
                let b = input[i];
                result.push(b);
                i += 1;
                if b == b'\\' {
                    if let Some(&b) = input.get(i) {
                        result.push(b);
                        i += 1;
                    }
                } else if b == b'"' {
                    break;
                }
            }
        } else if input[i..].starts_with(b"-0")
            && input
                .get(i + 2)
                .is_none_or(|b| !matches!(b, b'0'..=b'9' | b'.' | b'e' | b'E'))
        {
            result.push(b'0');
            i += 2;
        } else {
            result.push(input[i]);
            i += 1;
        }
    }
    result
}

// port: belongs in provenant-core
pub(crate) fn python_splitlines(text: &str, keepends: bool) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if matches!(
            ch,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            let mut end = index + ch.len_utf8();
            if ch == '\r' && chars.peek().is_some_and(|(_, c)| *c == '\n') {
                chars.next();
                end += 1;
            }
            out.push(&text[start..if keepends { end } else { index }]);
            start = end;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}
