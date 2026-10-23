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
/// Python int() for JSON inputs, followed by max(1, ...).
pub fn positive_timeout(value: &Value) -> Result<u64, IntegerError> {
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
            let value = provenant_core::py::python_strip(value);
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
    Ok(integer.max(1) as u64)
}

// port: belongs in provenant-core
/// Unicode decimal blocks (Nd); unlike char::to_digit this includes Python's digits.
pub(crate) fn decimal_digit(ch: char) -> Option<u8> {
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
