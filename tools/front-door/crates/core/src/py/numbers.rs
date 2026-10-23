//! Python-compatible parsing and numeric helpers shared by the port.

use std::cmp::Ordering;
use std::fmt;
use thiserror::Error;

/// An arbitrary-width decimal integer parsed using Python's `int(str)` rules.
///
/// The representation stays decimal so comparisons remain exact without
/// converting through `f64`. Materialize only when a caller needs a bounded
/// machine integer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Integer {
    negative: bool,
    digits: String,
}

fn numeric_whitespace(ch: char) -> bool {
    super::python_whitespace(ch) && !matches!(ch, '\u{1c}'..='\u{1f}')
}

impl Integer {
    /// Parse a decimal integer with Unicode decimal digits and Python's
    /// underscore and whitespace rules. Invalid text and Python's 4,300 digit
    /// limit return `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim_matches(numeric_whitespace);
        let (negative, text) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        let mut digits = String::new();
        let mut need_digit = true;
        for ch in text.chars() {
            if ch == '_' && !need_digit {
                need_digit = true;
                continue;
            }
            let digit = super::decimal_digit(ch)?;
            digits.push(char::from(b'0' + digit));
            need_digit = false;
        }
        if need_digit || digits.len() > 4_300 {
            return None;
        }
        let digits = digits.trim_start_matches('0');
        let digits = if digits.is_empty() { "0" } else { digits };
        Some(Self {
            negative: negative && digits != "0",
            digits: digits.to_owned(),
        })
    }

    /// Convert to `i32`, returning a typed error when the value does not fit.
    pub fn as_i32(&self) -> Result<i32, NumberOutOfRange> {
        self.to_string().parse().map_err(|_| NumberOutOfRange)
    }

    /// Convert to `u64`, returning a typed error when the value does not fit.
    pub fn as_u64(&self) -> Result<u64, NumberOutOfRange> {
        self.to_string().parse().map_err(|_| NumberOutOfRange)
    }

    /// Convert to `i64`, returning a typed error when the value does not fit.
    pub fn as_i64(&self) -> Result<i64, NumberOutOfRange> {
        self.to_string().parse().map_err(|_| NumberOutOfRange)
    }
}

impl fmt::Display for Integer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negative {
            f.write_str("-")?;
        }
        f.write_str(&self.digits)
    }
}

impl Ord for Integer {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => {
                let magnitude_order = self
                    .digits
                    .len()
                    .cmp(&other.digits.len())
                    .then_with(|| self.digits.cmp(&other.digits));
                if self.negative {
                    magnitude_order.reverse()
                } else {
                    magnitude_order
                }
            }
        }
    }
}

impl PartialOrd for Integer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A parsed decimal integer does not fit the requested machine type.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("integer is outside the supported range")]
pub struct NumberOutOfRange;

/// Failure while converting a floating-point value to its integral text.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum FloatWholeError {
    #[error("cannot convert a non-finite float to an integer")]
    NonFinite,
}

/// Python `int(float)` semantics: truncate the exact binary value toward zero.
pub fn float_whole(value: f64) -> Result<String, FloatWholeError> {
    let bits = value.to_bits();
    let exponent_bits = ((bits >> 52) & 0x7ff) as u16;
    if exponent_bits == 0x7ff {
        return Err(FloatWholeError::NonFinite);
    }
    let negative = bits >> 63 != 0;
    let exponent = i32::from(exponent_bits) - 1023;
    if exponent < 0 {
        return Ok("0".into());
    }
    let mantissa = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
    let digits = if exponent <= 52 {
        (mantissa >> (52 - exponent)).to_string()
    } else {
        let mut decimal: Vec<_> = mantissa
            .to_string()
            .bytes()
            .rev()
            .map(|byte| byte - b'0')
            .collect();
        for _ in 0..(exponent - 52) {
            let mut carry = 0;
            for digit in &mut decimal {
                let doubled = *digit * 2 + carry;
                *digit = doubled % 10;
                carry = doubled / 10;
            }
            if carry != 0 {
                decimal.push(carry);
            }
        }
        decimal
            .into_iter()
            .rev()
            .map(|digit| char::from(b'0' + digit))
            .collect()
    };
    if negative && digits != "0" {
        Ok(format!("-{digits}"))
    } else {
        Ok(digits)
    }
}

/// Errors for dividing a decimal string by a small integer.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum DecimalDivisionError {
    #[error("decimal input is empty")]
    EmptyInput,
    #[error("decimal input contains a non-digit")]
    InvalidDigit,
    #[error("division by zero")]
    DivisionByZero,
}

/// Divide a non-empty ASCII decimal string without materializing its value.
pub fn divmod_decimal(digits: &str, divisor: u32) -> Result<(String, u32), DecimalDivisionError> {
    if divisor == 0 {
        return Err(DecimalDivisionError::DivisionByZero);
    }
    if digits.is_empty() {
        return Err(DecimalDivisionError::EmptyInput);
    }
    let mut quotient = String::new();
    let mut remainder = 0_u64;
    let divisor = u64::from(divisor);
    for byte in digits.bytes() {
        if !byte.is_ascii_digit() {
            return Err(DecimalDivisionError::InvalidDigit);
        }
        remainder = remainder * 10 + u64::from(byte - b'0');
        let quotient_digit = remainder / divisor;
        remainder %= divisor;
        if quotient_digit != 0 || !quotient.is_empty() {
            // The long-division remainder is less than the divisor, so each
            // emitted quotient digit is in 0..=9.
            quotient.push(char::from(b'0' + quotient_digit as u8));
        }
    }
    if quotient.is_empty() {
        quotient.push('0');
    }
    Ok((quotient, remainder as u32))
}

/// Parse Python `float(str)` syntax, including Unicode decimal digits.
pub fn python_float(text: &str) -> Option<f64> {
    let text = text.trim_matches(numeric_whitespace);
    let chars: Vec<_> = text.chars().collect();
    let mut normalized = String::new();
    for (index, &ch) in chars.iter().enumerate() {
        if ch == '_' {
            let previous_is_digit = index
                .checked_sub(1)
                .and_then(|previous| chars.get(previous))
                .and_then(|&previous| super::decimal_digit(previous))
                .is_some();
            let next_is_digit = chars
                .get(index.checked_add(1)?)
                .and_then(|&next| super::decimal_digit(next))
                .is_some();
            if !previous_is_digit || !next_is_digit {
                return None;
            }
        } else if let Some(digit) = super::decimal_digit(ch) {
            normalized.push(char::from(b'0' + digit));
        } else if ch.is_ascii() {
            normalized.push(ch);
        } else {
            return None;
        }
    }
    let lower = normalized.to_ascii_lowercase();
    let (negative, body) = match lower.strip_prefix('-') {
        Some(body) => (true, body),
        None => (false, lower.strip_prefix('+').unwrap_or(&lower)),
    };
    match body {
        "nan" => Some(if negative { -f64::NAN } else { f64::NAN }),
        "inf" | "infinity" => Some(if negative {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }),
        _ => normalized.parse().ok(),
    }
}

/// Errors produced by POSIX `shlex.split(comments=False)` parsing.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ShellSplitError {
    #[error("No closing quotation")]
    UnclosedQuote,
    #[error("No escaped character")]
    TrailingEscape,
}

/// Split like Python's `shlex.split(text)` in POSIX mode, without comments.
pub fn shell_split(text: &str) -> Result<Vec<String>, ShellSplitError> {
    let mut chars = text.chars();
    let mut result = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') if ch != '\'' => word.push(ch),
            Some('"') if ch == '\\' => {
                let next = chars.next().ok_or(ShellSplitError::TrailingEscape)?;
                if next != '\\' && next != '"' {
                    word.push('\\');
                }
                word.push(next);
            }
            Some(q) if ch == q => quote = None,
            Some(_) => word.push(ch),
            None => match ch {
                ' ' | '\t' | '\r' | '\n' => {
                    if started {
                        result.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                '\'' | '"' => {
                    quote = Some(ch);
                    started = true;
                }
                '\\' => {
                    word.push(chars.next().ok_or(ShellSplitError::TrailingEscape)?);
                    started = true;
                }
                c => {
                    word.push(c);
                    started = true;
                }
            },
        }
    }
    if quote.is_some() {
        return Err(ShellSplitError::UnclosedQuote);
    }
    if started {
        result.push(word);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn python_oracle(body: &str, values: &Value) -> Option<Value> {
        crate::test_support::python_values(body, values)
    }

    #[test]
    fn integer_text_matches_python_decimal_parsing_and_normalization() {
        let long_valid = "1".repeat(4_300);
        let long_zero = "0".repeat(4_301);
        let mut inputs: Vec<String> = vec![
            "0",
            "-0",
            "+000",
            "  -1_234  ",
            "９_８７",
            "𝟠",
            "+٢_٠",
            "",
            "+",
            "_1",
            "1_",
            "1__2",
            "0x10",
            "1.0",
            "\u{1c}1",
            "1\u{2003}",
            long_valid.as_str(),
            long_zero.as_str(),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        inputs.extend(
            (0..=char::MAX as u32)
                .filter_map(char::from_u32)
                .filter(|&ch| super::super::python_whitespace(ch))
                .map(|ch| format!("{ch}17{ch}")),
        );
        let Some(expected) = python_oracle(
            r#"
import json
out=[]
for text in values:
    try: out.append(['ok', str(int(text))])
    except ValueError: out.append(['error'])
print(json.dumps(out))
"#,
            &json!(inputs),
        ) else {
            return;
        };
        let actual: Vec<_> = inputs
            .into_iter()
            .map(|text| match Integer::parse(&text) {
                Some(value) => json!(["ok", value.to_string()]),
                None => json!(["error"]),
            })
            .collect();
        assert_eq!(json!(actual), expected);
    }

    #[test]
    fn integer_materialization_reports_range_errors_without_float_rounding() {
        assert_eq!(
            Integer::parse("9223372036854775807").and_then(|n| n.as_i64().ok()),
            Some(i64::MAX)
        );
        assert_eq!(
            Integer::parse("9223372036854775808").map(|n| n.as_i64()),
            Some(Err(NumberOutOfRange))
        );
        assert_eq!(
            Integer::parse("18446744073709551615").map(|n| n.as_u64()),
            Some(Ok(u64::MAX))
        );
        assert_eq!(
            Integer::parse("18446744073709551616").map(|n| n.as_u64()),
            Some(Err(NumberOutOfRange))
        );
        assert_eq!(
            Integer::parse("-2147483649").map(|n| n.as_i32()),
            Some(Err(NumberOutOfRange))
        );
        assert_eq!(
            Integer::parse("2147483647").map(|n| n.as_i32()),
            Some(Ok(i32::MAX))
        );
        assert_eq!(
            Integer::parse("-9223372036854775808").map(|n| n.as_i64()),
            Some(Ok(i64::MIN))
        );
        assert_eq!(
            Integer::parse("-9223372036854775809").map(|n| n.as_i64()),
            Some(Err(NumberOutOfRange))
        );
    }

    #[test]
    fn arbitrary_integer_order_uses_signed_decimal_value() {
        let mut values: Vec<_> = ["-100", "-11", "-2", "-0", "0", "2", "11", "100"]
            .into_iter()
            .filter_map(Integer::parse)
            .collect();
        values.sort();
        assert_eq!(
            values.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["-100", "-11", "-2", "0", "0", "2", "11", "100"]
        );
    }

    #[test]
    fn float_text_matches_python_float_grammar_and_binary_results() {
        let mut inputs: Vec<String> = vec![
            "0",
            "-0.0",
            ".5",
            "1.",
            "1_2.3_4",
            "１.２e３",
            "1e+3",
            "-Infinity",
            "+inf",
            "NaN",
            "-nan",
            "1e400",
            "1__0",
            "_1.0",
            "1_.0",
            "1._0",
            "1e_2",
            "0x10",
            "1.2.3",
            "\u{2003}2\u{2003}",
            "\u{1c}2",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        inputs.extend(
            (0..=char::MAX as u32)
                .filter_map(char::from_u32)
                .filter(|&ch| super::super::python_whitespace(ch))
                .map(|ch| format!("{ch}2.5{ch}")),
        );
        let Some(expected) = python_oracle(
            r#"
import json, math, struct
out=[]
for text in values:
    try: number=float(text)
    except ValueError: out.append(None); continue
    if math.isnan(number): out.append('nan')
    elif math.isinf(number): out.append('-inf' if number < 0 else '+inf')
    else: out.append(struct.pack('>d', number).hex())
print(json.dumps(out))
"#,
            &json!(inputs),
        ) else {
            return;
        };
        let actual: Vec<_> = inputs
            .into_iter()
            .map(|text| match python_float(&text) {
                None => Value::Null,
                Some(number) if number.is_nan() => json!("nan"),
                Some(number) if number.is_infinite() => {
                    json!(if number.is_sign_negative() {
                        "-inf"
                    } else {
                        "+inf"
                    })
                }
                Some(number) => json!(format!("{:016x}", number.to_bits())),
            })
            .collect();
        assert_eq!(json!(actual), expected);
    }

    #[test]
    fn float_whole_matches_python_int_for_finite_binary64_values() {
        let inputs = [
            -0.0,
            -0.9,
            -1.9,
            0.9,
            1.9,
            60.999,
            1.0e20,
            1.0e100,
            f64::MAX,
        ];
        let Some(expected) = python_oracle(
            r#"
import json
print(json.dumps([str(int(value)) for value in values]))
"#,
            &json!(inputs),
        ) else {
            return;
        };
        let actual: Vec<_> = inputs
            .into_iter()
            .map(|value| float_whole(value).expect("finite float"))
            .collect();
        assert_eq!(json!(actual), expected);
        assert_eq!(float_whole(f64::INFINITY), Err(FloatWholeError::NonFinite));
        assert_eq!(float_whole(f64::NAN), Err(FloatWholeError::NonFinite));
    }

    #[test]
    fn decimal_division_handles_long_inputs_and_rejects_malformed_operands() {
        let cases = [
            ("12345678901234567890", 60),
            ("000", 7),
            ("999999999999999999999999999999999999", u32::MAX),
        ];
        let Some(expected) = python_oracle(
            r#"
import json
print(json.dumps([[str(divmod(int(digits), divisor)[0]), divmod(int(digits), divisor)[1]] for digits, divisor in values]))
"#,
            &json!(cases),
        ) else {
            return;
        };
        let actual: Vec<_> = cases
            .into_iter()
            .map(|(digits, divisor)| {
                let (quotient, remainder) = divmod_decimal(digits, divisor).expect("valid decimal");
                json!([quotient, remainder])
            })
            .collect();
        assert_eq!(json!(actual), expected);
        assert_eq!(divmod_decimal("000", 7), Ok(("0".into(), 0)));
        assert_eq!(divmod_decimal("", 7), Err(DecimalDivisionError::EmptyInput));
        assert_eq!(
            divmod_decimal("12x", 7),
            Err(DecimalDivisionError::InvalidDigit)
        );
        assert_eq!(
            divmod_decimal("12", 0),
            Err(DecimalDivisionError::DivisionByZero)
        );
    }

    #[test]
    fn shell_split_matches_python_posix_shlex_including_errors() {
        let inputs = [
            "",
            "  a\tb\n c  ",
            "cmd --cd '/repo/a b'",
            "a''b \"\"",
            "a\\ b",
            "\"a\\$b\"",
            "\"a\\\"b\"",
            "\"a\\\\b\"",
            "\"a\\qb\"",
            "'a\\b'",
            "a\\\nb",
            "a#b",
            "a\u{2003}b",
            "'unfinished",
            "trailing\\",
            "\"trailing\\",
        ];
        let Some(expected) = python_oracle(
            r#"
import json, shlex
out=[]
for text in values:
    try: out.append({'ok': shlex.split(text)})
    except ValueError as error: out.append({'error': str(error)})
print(json.dumps(out))
"#,
            &json!(inputs),
        ) else {
            return;
        };
        let actual: Vec<_> = inputs
            .into_iter()
            .map(|text| match shell_split(text) {
                Ok(parts) => json!({"ok": parts}),
                Err(error) => json!({"error": error.to_string()}),
            })
            .collect();
        assert_eq!(json!(actual), expected);
    }
}

/// Errors for a prevalidated Unicode decimal token (for example Python re `\d+`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IntegerError {
    #[error("Exceeds the limit (4300 digits) for integer string conversion: value has {digits} digits; use sys.set_int_max_str_digits() to increase the limit")]
    DigitLimit { digits: usize },
    #[error("integer outside i64/u64 range")]
    NumberOutOfRange,
}

/// Convert a nonempty Unicode decimal token to u64. Signs, underscores and
/// whitespace belong to `Integer::parse`, not this already-tokenized interface.
pub fn decimal_integer(text: &str) -> Result<u64, IntegerError> {
    let digits = text.chars().count();
    if digits > 4300 {
        return Err(IntegerError::DigitLimit { digits });
    }
    if digits == 0 || !text.chars().all(|c| super::decimal_digit(c).is_some()) {
        return Err(IntegerError::NumberOutOfRange);
    }
    Integer::parse(text)
        .ok_or(IntegerError::NumberOutOfRange)?
        .as_u64()
        .map_err(|_| IntegerError::NumberOutOfRange)
}

#[cfg(test)]
mod decimal_token_tests {
    use super::*;
    #[test]
    fn decimal_tokens_match_python_digit_limit_and_exact_u64_boundaries() {
        let inputs = [
            "0".to_string(),
            "٠٢٤".into(),
            "９００７１９９２５４７４０９９３".into(),
            u64::MAX.to_string(),
            "18446744073709551616".into(),
            "0".repeat(4300),
            "7".repeat(4301),
            "0".repeat(4301),
        ];
        let Some(expected) = crate::test_support::python_values(
            r#"
out=[]
for s in values:
    try:
        n=int(s)
        out.append(['range'] if n>2**64-1 else ['ok',n])
    except ValueError as e: out.append(['error',str(e)])
print(json.dumps(out))
"#,
            &serde_json::json!(inputs),
        ) else {
            return;
        };
        let actual: Vec<_> = inputs
            .iter()
            .map(|s| match decimal_integer(s) {
                Ok(n) => serde_json::json!(["ok", n]),
                Err(IntegerError::NumberOutOfRange) => serde_json::json!(["range"]),
                Err(e) => serde_json::json!(["error", e.to_string()]),
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
        for invalid in ["", "+1", "1_0", "²", "1x"] {
            assert!(decimal_integer(invalid).is_err());
        }
    }
}
