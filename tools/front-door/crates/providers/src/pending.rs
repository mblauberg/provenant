//! Private compatibility primitives awaiting their owning crate.
use serde_json::Value;

// port: belongs in provenant-core
// Extension of the CLI pending compatibility module; no JSON truth/equality
// primitive exists there yet. Keep integers exact throughout comparisons.
pub(crate) fn python_truth(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                value != 0
            } else if let Some(value) = value.as_u64() {
                value != 0
            } else {
                value.as_f64().is_some_and(|value| value != 0.0)
            }
        }
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

// port: belongs in provenant-core
pub(crate) fn python_json_eq(left: &Value, right: &Value) -> bool {
    fn integer(value: &Value) -> Option<i128> {
        match value {
            Value::Bool(value) => Some(i128::from(*value)),
            Value::Number(number) => number
                .as_i64()
                .map(i128::from)
                .or_else(|| number.as_u64().map(i128::from))
                .or_else(|| {
                    let float = number.as_f64()?;
                    // Only integral floats in the JSON integer domain can equal
                    // an integer. Conversion goes float -> integer, never back.
                    (float.fract() == 0.0
                        && (-9_223_372_036_854_775_808.0..18_446_744_073_709_551_616.0)
                            .contains(&float))
                    .then_some(float as i128)
                }),
            _ => None,
        }
    }
    match (left, right) {
        (Value::Bool(_) | Value::Number(_), Value::Bool(_) | Value::Number(_)) => {
            match (integer(left), integer(right)) {
                (Some(left), Some(right)) => left == right,
                _ => left == right,
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(l, r)| python_json_eq(l, r))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .all(|(key, l)| right.get(key).is_some_and(|r| python_json_eq(l, r)))
        }
        _ => left == right,
    }
}

// port: belongs in provenant-core
// datetime.fromisoformat truncates arbitrary fractional precision to six
// digits, including an offset's fraction. Jiff already accepts basic ISO forms.
// Remaining parser differences are tracked in the divergence log.
pub(crate) fn python_iso_timestamp(value: &str) -> Option<jiff::Timestamp> {
    let mut normalized = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        normalized.push(if c == ',' { '.' } else { c });
        if c == '.' || c == ',' {
            let mut count = 0;
            while chars.peek().is_some_and(char::is_ascii_digit) {
                let digit = chars.next()?;
                if count < 6 {
                    normalized.push(digit);
                }
                count += 1;
            }
        }
    }
    // Jiff rounds fractional UTC offsets to whole seconds; Python preserves
    // microseconds. Parse the integral offset, then apply the fraction exactly.
    let offset = normalized.rfind(['+', '-']).filter(|&index| index > 9);
    let mut offset_micros = 0i64;
    if let Some(offset) = offset {
        if let Some(dot) = normalized[offset..].find('.') {
            let dot = offset + dot;
            let integral = &normalized[offset + 1..dot];
            // CPython requires explicit offset seconds before a fraction.
            let extended = integral.len() == 8
                && integral.as_bytes()[2] == b':'
                && integral.as_bytes()[5] == b':'
                && integral
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| i == 2 || i == 5 || b.is_ascii_digit());
            let basic = integral.len() == 6 && integral.bytes().all(|b| b.is_ascii_digit());
            if !extended && !basic {
                return None;
            }
            let fraction = normalized.get(dot + 1..)?;
            if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            offset_micros = fraction.parse::<i64>().ok()?
                * 10i64.pow(6u32.checked_sub(fraction.len().try_into().ok()?)?);
            if normalized.as_bytes()[offset] == b'-' {
                offset_micros = -offset_micros;
            }
            normalized.truncate(dot);
        }
    }
    let timestamp: jiff::Timestamp = normalized.parse().ok()?;
    timestamp
        .checked_sub(jiff::SignedDuration::from_micros(offset_micros))
        .ok()
}

// port: belongs in provenant-records
// Preflight integer tokens before serde_json can convert an oversized integer
// to f64. Decimal/exponent tokens are Python floats and remain floats. Strings
// are opaque, including escaped quotes and integer-looking credential values.
pub(crate) fn check_json_integer_ranges(input: &[u8]) -> Result<(), crate::DecodeError> {
    let mut index = 0;
    while index < input.len() {
        match input[index] {
            b'"' => {
                index += 1;
                while index < input.len() {
                    match input[index] {
                        b'\\' => {
                            index += 1;
                        }
                        b'"' => {
                            index += 1;
                            break;
                        }
                        _ => {}
                    }
                    index += 1;
                }
            }
            b'-' | b'0'..=b'9' => {
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
                    // All bytes are ASCII here. Invalid JSON syntax remains
                    // serde's responsibility after this numeric-range guard.
                    let token = std::str::from_utf8(token)
                        .map_err(|_| crate::DecodeError::NumberOutOfRange)?;
                    let fits = if token.starts_with('-') {
                        token.parse::<i64>().is_ok()
                    } else {
                        token.parse::<u64>().is_ok()
                    };
                    if !fits {
                        return Err(crate::DecodeError::NumberOutOfRange);
                    }
                }
            }
            _ => index += 1,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_timestamp_conversion_matches_cpython_without_float_seconds() {
        let stamps = serde_json::json!([
            "20261023T120000.123456Z",
            "2026-10-23T12:00:00.123456999999Z",
            "2026-10-23T12:00:00,123456999Z",
            // Sub-second UTC offsets (+00:00:00.500000) are left out: CPython
            // 3.14 patch releases disagree on them, so no oracle answer is stable.
            "2026-10-23T12:00:00-00:00:00.500000",
            "2026-10-23T12:00:00+05:30",
            "2026-10-23T12:00:00+01:02.500000",
            "2026-10-23T12:00:00+01.500000",
            "2026-10-23T12:00:00+0102.500000",
            "2026-10-23T12:00:00",
            "bad"
        ]);
        let Some(expected): Option<Vec<Option<i64>>> = crate::oracle::python(
            r#"
import json, sys
from datetime import UTC, datetime
out = []
for s in json.load(sys.stdin):
    try:
        dt = datetime.fromisoformat(s.replace("Z", "+00:00"))
        if dt.tzinfo is None: raise ValueError()
        delta = dt.astimezone(UTC) - datetime(1970, 1, 1, tzinfo=UTC)
        out.append((delta.days * 86400 + delta.seconds) * 1000000 + delta.microseconds)
    except ValueError:
        out.append(None)
print(json.dumps(out))
"#,
            &stamps,
            None,
        ) else {
            return;
        };
        for (stamp, expected) in stamps.as_array().unwrap().iter().zip(expected) {
            let actual = python_iso_timestamp(stamp.as_str().unwrap()).map(|t| t.as_microsecond());
            assert_eq!(actual, expected, "{stamp}");
        }
    }
}
