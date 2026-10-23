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

// port: belongs in provenant-core
pub(crate) fn value_truthy(value: &serde_json::Value) -> bool {
    truthy(&crate::ordered_json::OrderedValue::from(value.clone()))
}

// port: belongs in provenant-core
pub(crate) fn decimal_digit(c: char) -> Option<char> {
    let n = c as u32;
    const STARTS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450,
        0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50,
        0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce,
        0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
    ];
    STARTS.iter().find_map(|start| {
        n.checked_sub(*start)
            .filter(|d| *d < 10)
            .and_then(|d| char::from_u32(48 + d))
    })
}

// port: belongs in provenant-core
pub(crate) fn python_one(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(Value::Bool(v)) => *v,
        Some(Value::Number(n)) if n.is_f64() => n.as_f64() == Some(1.0),
        Some(Value::Number(n)) => n.as_i64() == Some(1) || n.as_u64() == Some(1),
        _ => false,
    }
}

// port: belongs in provenant-core
pub(crate) fn timestamp(raw: &str) -> Option<jiff::Timestamp> {
    use jiff::civil::{Date, ISOWeekDate, Time, Weekday};
    let normalized = raw.replace('Z', "+00:00");
    let raw = normalized.as_str();
    let pattern=regex::Regex::new(r"(?s)^([0-9]{4}-[0-9]{2}-[0-9]{2}|[0-9]{8}|[0-9]{4}-W[0-9]{2}(?:-[1-7])?|[0-9]{4}W[0-9]{2}[1-7]?)(.)(.+)$").ok()?;
    let captures = pattern.captures(raw)?;
    let date = captures.get(1)?.as_str();
    let year = date.get(..4)?.parse::<i16>().ok()?;
    if !(1..=9999).contains(&year) {
        return None;
    }
    let mut date = if let Some((_, week)) = date.split_once('W') {
        let compact = week.replace('-', "");
        let number = compact.get(..2)?.parse().ok()?;
        let weekday = match compact.get(2..).unwrap_or("") {
            "" | "1" => Weekday::Monday,
            "2" => Weekday::Tuesday,
            "3" => Weekday::Wednesday,
            "4" => Weekday::Thursday,
            "5" => Weekday::Friday,
            "6" => Weekday::Saturday,
            "7" => Weekday::Sunday,
            _ => return None,
        };
        Date::from_iso_week_date(ISOWeekDate::new(year, number, weekday).ok()?)
    } else {
        let compact = date.replace('-', "");
        Date::new(
            year,
            compact.get(4..6)?.parse().ok()?,
            compact.get(6..8)?.parse().ok()?,
        )
        .ok()?
    };
    let time = captures.get(3)?.as_str();
    let (clock, offset) = if let Some(clock) = time.strip_suffix('Z') {
        (clock, 0i64)
    } else {
        let (index, sign) = time.char_indices().find(|(_, c)| matches!(c, '+' | '-'))?;
        let clock = time.get(..index)?;
        let (h, m, s, us) = iso_time(time.get(index + 1..)?)?;
        let seconds = i64::from(h) * 3600 + i64::from(m) * 60 + i64::from(s);
        let offset = seconds * 1_000_000 + i64::from(us);
        if offset >= 86_400_000_000 {
            return None;
        }
        (clock, if sign == '-' { -offset } else { offset })
    };
    let (h, m, s, us) = iso_time(clock)?;
    let h = if h == 24 && m == 0 && s == 0 && us == 0 {
        use jiff::ToSpan;
        date = date.checked_add(1.day()).ok()?;
        0
    } else {
        h
    };
    let time = Time::new(
        h.try_into().ok()?,
        m.try_into().ok()?,
        s.try_into().ok()?,
        us * 1000,
    )
    .ok()?;
    let utc = jiff::tz::Offset::UTC
        .to_timestamp(date.to_datetime(time))
        .ok()?;
    let micros = i64::try_from(utc.as_nanosecond().div_euclid(1000))
        .ok()?
        .checked_sub(offset)?;
    jiff::Timestamp::from_microsecond(micros).ok()
}

// port: belongs in provenant-core
fn iso_time(raw: &str) -> Option<(u8, u8, u8, i32)> {
    let (main, fraction) = raw
        .split_once(['.', ','])
        .map(|(m, f)| (m, Some(f)))
        .unwrap_or((raw, None));
    let parts: Vec<_> = if main.contains(':') {
        main.split(':').collect()
    } else {
        if !main.is_ascii() || ![2, 4, 6].contains(&main.len()) {
            return None;
        }
        main.as_bytes()
            .chunks(2)
            .map(std::str::from_utf8)
            .collect::<Result<Vec<_>, _>>()
            .ok()?
    };
    if parts.is_empty()
        || parts.len() > 3
        || parts
            .iter()
            .any(|p| p.len() != 2 || !p.bytes().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let h = parts.first()?.parse().ok()?;
    let m = parts
        .get(1)
        .map(|p| p.parse())
        .transpose()
        .ok()?
        .unwrap_or(0);
    let s = parts
        .get(2)
        .map(|p| p.parse())
        .transpose()
        .ok()?
        .unwrap_or(0);
    if fraction.is_some() && parts.len() != 3 {
        return None;
    }
    let us = if let Some(f) = fraction {
        if f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let mut digits = f.chars().take(6).collect::<String>();
        while digits.len() < 6 {
            digits.push('0');
        }
        digits.parse().ok()?
    } else {
        0
    };
    Some((h, m, s, us))
}

// port: belongs in provenant-core
pub(crate) fn ordered_string(value: &crate::ordered_json::OrderedValue) -> String {
    use crate::ordered_json::OrderedValue;
    fn repr(value: &OrderedValue) -> String {
        match value {
            OrderedValue::String(s) => python_repr(s),
            _ => ordered_string(value),
        }
    }
    match value {
        OrderedValue::Array(items) => format!(
            "[{}]",
            items.iter().map(repr).collect::<Vec<_>>().join(", ")
        ),
        OrderedValue::Object(items) => format!(
            "{{{}}}",
            items
                .0
                .iter()
                .map(|(k, v)| format!("{}: {}", python_repr(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => python_string(&value.to_value()),
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
