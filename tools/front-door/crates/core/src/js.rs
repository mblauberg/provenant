//! ECMAScript string and number behaviour shared by Rust ports.

use thiserror::Error;

/// The error returned when a millisecond value is outside JavaScript's
/// `Date` TimeClip range.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum JsError {
    #[error("millisecond timestamp is outside the ECMAScript TimeClip range: {0}")]
    TimeClip(i64),
}

/// Whether a character is in ECMAScript's `WhiteSpace` or `LineTerminator`
/// set, the characters removed by `String.prototype.trim`.
pub fn js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// Remove the ECMAScript whitespace characters from both ends of `value`.
pub fn trim_ecmascript(value: &str) -> &str {
    value.trim_matches(js_whitespace)
}

/// Parse a string with JavaScript's `Number(string)` grammar.
///
/// `None` represents JavaScript's `NaN` result. Empty or whitespace-only
/// strings produce positive zero; the exact `Infinity` spellings are kept.
pub fn js_number(value: &str) -> Option<f64> {
    let value = trim_ecmascript(value);
    if value.is_empty() {
        return Some(0.0);
    }
    match value {
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }

    if let Some((radix, digits)) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map(|digits| (16, digits))
        .or_else(|| {
            value
                .strip_prefix("0b")
                .or_else(|| value.strip_prefix("0B"))
                .map(|digits| (2, digits))
        })
        .or_else(|| {
            value
                .strip_prefix("0o")
                .or_else(|| value.strip_prefix("0O"))
                .map(|digits| (8, digits))
        })
    {
        if digits.is_empty() {
            return None;
        }
        return radix_integer(digits, radix);
    }

    if !is_ecmascript_decimal(value) {
        return None;
    }
    value.parse::<f64>().ok().filter(|number| !number.is_nan())
}

fn is_ecmascript_decimal(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut i = if bytes
        .first()
        .is_some_and(|byte| matches!(*byte, b'+' | b'-'))
    {
        1
    } else {
        0
    };
    let int_start = i;
    while bytes.get(i).is_some_and(|byte| byte.is_ascii_digit()) {
        i += 1;
    }
    let has_integer_digits = i > int_start;
    let mut has_fraction_digits = false;
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        let fraction_start = i;
        while bytes.get(i).is_some_and(|byte| byte.is_ascii_digit()) {
            i += 1;
        }
        has_fraction_digits = i > fraction_start;
    }
    if !has_integer_digits && !has_fraction_digits {
        return false;
    }
    if bytes
        .get(i)
        .is_some_and(|byte| matches!(*byte, b'e' | b'E'))
    {
        i += 1;
        if bytes
            .get(i)
            .is_some_and(|byte| matches!(*byte, b'+' | b'-'))
        {
            i += 1;
        }
        let exponent_start = i;
        while bytes.get(i).is_some_and(|byte| byte.is_ascii_digit()) {
            i += 1;
        }
        if i == exponent_start {
            return false;
        }
    }
    i == bytes.len()
}

// Radix prefixes accepted by Number are powers of two. Keep the leading 54
// bits and a sticky bit so arbitrarily long inputs round exactly to binary64.
fn radix_integer(digits: &str, radix: u32) -> Option<f64> {
    let width = radix.trailing_zeros() as usize;
    let mut started = false;
    let mut bit_len = 0usize;
    let mut leading = 0u64;
    let mut guard = false;
    let mut sticky = false;

    for byte in digits.bytes() {
        let digit = match byte {
            b'0'..=b'9' => u32::from(byte - b'0'),
            b'a'..=b'f' => u32::from(byte - b'a' + 10),
            b'A'..=b'F' => u32::from(byte - b'A' + 10),
            _ => return None,
        };
        if digit >= radix {
            return None;
        }
        for shift in (0..width).rev() {
            let bit = (digit >> shift) & 1 != 0;
            if !started {
                if !bit {
                    continue;
                }
                started = true;
            }
            bit_len = bit_len.saturating_add(1);
            match bit_len {
                1..=53 => leading = (leading << 1) | bit as u64,
                54 => guard = bit,
                _ => sticky |= bit,
            }
        }
    }

    if !started {
        return Some(0.0);
    }
    if bit_len > 1024 {
        return Some(f64::INFINITY);
    }

    let mut exponent = bit_len - 1;
    let mut significand = if bit_len <= 53 {
        leading << (53 - bit_len)
    } else {
        if guard && (sticky || leading & 1 == 1) {
            leading += 1;
        }
        leading
    };

    if significand == (1_u64 << 53) {
        significand >>= 1;
        exponent += 1;
    }
    if exponent > 1023 {
        return Some(f64::INFINITY);
    }

    let exponent_bits = (exponent as u64 + 1023) << 52;
    let fraction_bits = significand - (1_u64 << 52);
    Some(f64::from_bits(exponent_bits | fraction_bits))
}

/// Return the first `end` UTF-16 code units, as used by `String.slice(0, end)`.
/// A split surrogate pair becomes U+FFFD when converted to Rust's UTF-8 string.
pub fn js_slice(value: &str, end: usize) -> String {
    String::from_utf16_lossy(&value.encode_utf16().take(end).collect::<Vec<_>>())
}

/// Format an integral millisecond timestamp like `Date(ms).toISOString()`.
///
/// The valid TimeClip range is inclusive at ±8.64e15 ms. Years 0000–9999 use
/// four digits; years outside that range use a sign and six digits.
pub fn js_iso(ms: i64) -> Result<String, JsError> {
    if !(-8_640_000_000_000_000..=8_640_000_000_000_000).contains(&ms) {
        return Err(JsError::TimeClip(ms));
    }
    // Gregorian civil date from days since 1970-01-01. Euclidean division
    // gives the right day and time for negative timestamps as well.
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    let year = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{year:+07}")
    };
    let day_ms = ms.rem_euclid(86_400_000);
    let hour = day_ms / 3_600_000;
    let minute = day_ms / 60_000 % 60;
    let second = day_ms / 1000 % 60;
    let millis = day_ms % 1000;
    Ok(format!(
        "{year}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn number_bits(value: Option<f64>) -> Option<String> {
        value.map(|number| format!("{:016x}", number.to_bits()))
    }

    #[test]
    fn ecmascript_whitespace_and_trim_edges() {
        let yes = [
            '\u{0009}', '\u{000a}', '\u{000b}', '\u{000c}', '\u{000d}', '\u{0020}', '\u{00a0}',
            '\u{1680}', '\u{2000}', '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}',
            '\u{2006}', '\u{2007}', '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}',
            '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
        ];
        for c in yes {
            assert!(js_whitespace(c), "U+{:04X}", c as u32);
            assert_eq!(trim_ecmascript(&format!("{c}x{c}")), "x");
        }
        for c in ['\u{0085}', '\u{180e}', '\u{200b}', '\u{fe0f}'] {
            assert!(!js_whitespace(c), "U+{:04X}", c as u32);
            assert_eq!(trim_ecmascript(&format!("{c}x{c}")), format!("{c}x{c}"));
        }
    }

    #[test]
    fn number_grammar_radix_rounding_and_nonfinite_cases() {
        let cases = [
            "",
            " \t\u{feff}",
            "0",
            "-0",
            "+1",
            ".5",
            "1.",
            "1e+2",
            "01",
            "0x0",
            "0Xff",
            "0b101",
            "0o17",
            "0x20000000000001",
            "0x20000000000003",
            "0x10000000000000000",
            "0x1fffffffffffff",
            "0x1fffffffffffff0",
            "Infinity",
            "+Infinity",
            "-Infinity",
            "NaN",
            "inf",
            "+0x1",
            "-0b1",
            "0x",
            "0b2",
            "1_0",
            "1e",
            ".",
            "１２",
        ];
        for case in cases {
            let expected = match case {
                "" | " \t\u{feff}" | "0" => Some(0.0),
                "-0" => Some(-0.0),
                "+1" | "01" => Some(1.0),
                ".5" => Some(0.5),
                "1." => Some(1.0),
                "1e+2" => Some(100.0),
                "0x0" => Some(0.0),
                "0Xff" => Some(255.0),
                "0b101" => Some(5.0),
                "0o17" => Some(15.0),
                "0x20000000000001" => Some(9_007_199_254_740_992.0),
                "0x20000000000003" => Some(9_007_199_254_740_996.0),
                "0x10000000000000000" => Some(18_446_744_073_709_551_616.0),
                "0x1fffffffffffff" => Some(9_007_199_254_740_991.0),
                "0x1fffffffffffff0" => Some(144_115_188_075_855_856.0),
                "Infinity" | "+Infinity" => Some(f64::INFINITY),
                "-Infinity" => Some(f64::NEG_INFINITY),
                _ => None,
            };
            assert_eq!(
                number_bits(js_number(case)),
                number_bits(expected),
                "{case:?}"
            );
        }
        assert!(js_number("-0").unwrap().is_sign_negative());
    }

    #[test]
    fn utf16_slice_replaces_split_surrogate_pairs() {
        assert_eq!(js_slice("a😀b", 0), "");
        assert_eq!(js_slice("a😀b", 1), "a");
        assert_eq!(js_slice("a😀b", 2), "a�");
        assert_eq!(js_slice("a😀b", 3), "a😀");
        assert_eq!(js_slice("a😀b", 4), "a😀b");
        assert_eq!(js_slice("a😀b", usize::MAX), "a😀b");
    }

    #[test]
    fn iso_uses_timeclip_and_expanded_years() {
        assert_eq!(js_iso(0).unwrap(), "1970-01-01T00:00:00.000Z");
        assert_eq!(js_iso(-1).unwrap(), "1969-12-31T23:59:59.999Z");
        assert_eq!(
            js_iso(253_402_300_800_000).unwrap(),
            "+010000-01-01T00:00:00.000Z"
        );
        assert_eq!(
            js_iso(-62_167_219_200_000).unwrap(),
            "0000-01-01T00:00:00.000Z"
        );
        assert_eq!(
            js_iso(8_640_000_000_000_000).unwrap(),
            "+275760-09-13T00:00:00.000Z"
        );
        assert_eq!(
            js_iso(-8_640_000_000_000_000).unwrap(),
            "-271821-04-20T00:00:00.000Z"
        );
        assert_eq!(
            js_iso(8_640_000_000_000_001),
            Err(JsError::TimeClip(8_640_000_000_000_001))
        );
        assert_eq!(
            js_iso(-8_640_000_000_000_001),
            Err(JsError::TimeClip(-8_640_000_000_000_001))
        );
    }

    #[test]
    fn node_differential_for_ecmascript_helpers() {
        let whitespace = json!([
            "\u{0009}", "\u{000a}", "\u{000b}", "\u{000c}", "\u{000d}", "\u{0020}", "\u{00a0}",
            "\u{1680}", "\u{2000}", "\u{2001}", "\u{2002}", "\u{2003}", "\u{2004}", "\u{2005}",
            "\u{2006}", "\u{2007}", "\u{2008}", "\u{2009}", "\u{200a}", "\u{2028}", "\u{2029}",
            "\u{202f}", "\u{205f}", "\u{3000}", "\u{feff}", "\u{0085}", "\u{180e}", "\u{200b}",
            "\u{fe0f}"
        ]);
        let expected = whitespace
            .as_array()
            .unwrap()
            .iter()
            .map(|value| {
                let character = value.as_str().unwrap().chars().next().unwrap();
                json!([
                    js_whitespace(character),
                    trim_ecmascript(&format!("{character}x{character}"))
                ])
            })
            .collect::<Vec<_>>();
        let Some(oracle) = crate::test_support::node_values(
            "console.log(JSON.stringify(values.map(c => [/[\\s\\uFEFF]/u.test(c), (c + 'x' + c).trim()])));",
            &whitespace,
        ) else {
            return;
        };
        assert_eq!(oracle, json!(expected));

        let mut numbers: Vec<String> = [
            "",
            " \t\u{feff}",
            "0",
            "-0",
            "+1",
            ".5",
            "1.",
            "1e+2",
            "01",
            "0x0",
            "0Xff",
            "0b101",
            "0o17",
            "0x20000000000001",
            "0x20000000000003",
            "0x20000000000005",
            "0x20000000000007",
            "0x10000000000000000",
            "0x1fffffffffffff",
            "0x1fffffffffffff0",
            "Infinity",
            "+Infinity",
            "-Infinity",
            "NaN",
            "inf",
            "+0x1",
            "-0b1",
            "0x",
            "0b2",
            "1_0",
            "1e",
            ".",
            "１２",
            "1e9999",
            "1e-9999",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        numbers.push(format!("0x{}", "f".repeat(255)));
        numbers.push(format!("0x{}", "f".repeat(256)));
        numbers.push(format!("0x1{}", "0".repeat(255)));
        numbers.push(format!("0x{}1", "0".repeat(250)));
        numbers.push(format!("0x20000000000001{}", "0".repeat(100)));
        numbers.push(format!("0x20000000000003{}", "0".repeat(100)));
        let numbers = json!(numbers);
        let expected = numbers
            .as_array()
            .unwrap()
            .iter()
            .map(|value| json!(number_bits(js_number(value.as_str().unwrap()))))
            .collect::<Vec<_>>();
        let Some(oracle) = crate::test_support::node_values(
            "const bits = n => { if (Number.isNaN(n)) return null; const b = new ArrayBuffer(8); new DataView(b).setFloat64(0, n, false); return [...new Uint8Array(b)].map(x => x.toString(16).padStart(2, '0')).join(''); }; console.log(JSON.stringify(values.map(v => bits(Number(v)))));",
            &numbers,
        ) else {
            return;
        };
        assert_eq!(oracle, json!(expected));

        let slices = json!([
            {"text":"a😀b", "end":0}, {"text":"a😀b", "end":1},
            {"text":"a😀b", "end":2}, {"text":"a😀b", "end":3},
            {"text":"a😀b", "end":4}, {"text":"𐐷x", "end":1},
            {"text":"𐐷x", "end":2}, {"text":"abc", "end":99}
        ]);
        let expected = slices
            .as_array()
            .unwrap()
            .iter()
            .map(|value| {
                json!(js_slice(
                    value["text"].as_str().unwrap(),
                    value["end"].as_u64().unwrap() as usize
                ))
            })
            .collect::<Vec<_>>();
        let Some(oracle) = crate::test_support::node_values(
            "console.log(JSON.stringify(values.map(v => new TextDecoder().decode(new TextEncoder().encode(v.text.slice(0, v.end))))));",
            &slices,
        ) else {
            return;
        };
        assert_eq!(oracle, json!(expected));

        let mut millis = vec![
            -8_640_000_000_000_001_i64,
            -8_640_000_000_000_000_i64,
            -62_167_219_200_000_i64,
            -1_i64,
            0_i64,
            1_i64,
            253_402_300_800_000_i64,
            8_640_000_000_000_000_i64,
            8_640_000_000_000_001_i64,
        ];
        for anchor in [
            -8_640_000_000_000_000_i64,
            -62_167_219_200_000_i64,
            0,
            253_402_300_800_000,
            8_640_000_000_000_000,
        ] {
            for delta in [-86_400_001_i64, -1, 0, 1, 86_400_001] {
                millis.push(anchor + delta);
            }
        }
        // Seeded timestamps cover the full clipped range reproducibly.
        let span = 17_280_000_000_000_001_u64;
        let mut seed = 0x6a09_e667_f3bc_c909_u64;
        for _ in 0..64 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            millis.push((seed % span) as i64 - 8_640_000_000_000_000);
        }
        let millis = json!(millis);
        let expected = millis
            .as_array()
            .unwrap()
            .iter()
            .map(|value| js_iso(value.as_i64().unwrap()).ok())
            .collect::<Vec<_>>();
        let Some(oracle) = crate::test_support::node_values(
            "console.log(JSON.stringify(values.map(ms => { try { return new Date(ms).toISOString(); } catch { return null; } })));",
            &millis,
        ) else {
            return;
        };
        assert_eq!(oracle, json!(expected));
    }
}
