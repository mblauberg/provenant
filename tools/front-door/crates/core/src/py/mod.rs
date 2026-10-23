//! CPython compatibility for JSON-shaped values and timestamps.
//! Integer JSON decoding stays with `provenant_records::checked_json`.
use serde_json::Value;
mod datetime;
pub mod numbers;
pub use numbers::{
    DecimalDivisionError, FloatWholeError, Integer, IntegerError, NumberOutOfRange,
    ShellSplitError, decimal_integer, divmod_decimal, float_whole, python_float, shell_split,
};
pub mod unicode;
pub use datetime::{
    DateTimeError, IsoformatSemantics, PythonDateTime, PythonTimestamp, from_timestamp, iso,
    parse_time, python_fromisoformat, python_fromisoformat_with, python_iso_timestamp,
};
pub use unicode::{
    decimal_digit, printable, python_alpha, python_digit, python_numeric, python_whitespace,
    python_word, python_word_character,
};
pub fn python_truth(value: &Value) -> bool {
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

pub fn python_json_eq(left: &Value, right: &Value) -> bool {
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

pub fn python_strip(text: &str) -> &str {
    text.trim_matches(python_whitespace)
}

pub fn python_repr(text: &str) -> String {
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
            c if !unicode::printable(c) => {
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

/// Python's str/repr share shortest float digits but differ from JSON's notation.
pub fn python_number_str(number: &serde_json::Number) -> String {
    if !number.is_f64() {
        return number.to_string();
    }
    // Keep serde's correctly rounded shortest digits; Rust LowerExp can choose
    // the other decimal at a shortest-representation tie (e.g. ...851.125).
    let text = number.to_string();
    let (mantissa, exponent) = text.split_once('e').map_or((text.as_str(), 0), |(m, e)| {
        (m, e.parse::<i32>().unwrap_or(0))
    });
    let (sign, mantissa) = mantissa
        .strip_prefix('-')
        .map_or(("", mantissa), |m| ("-", m));
    let mut point = mantissa.find('.').unwrap_or(mantissa.len()) as i32 + exponent;
    let digits = mantissa.replace('.', "");
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return format!("{sign}0.0");
    }
    point -= (digits.len() - significant.len()) as i32;
    let digits = significant.trim_end_matches('0');
    let exponent = point - 1;
    if !(-4..16).contains(&exponent) {
        let (first, rest) = digits.split_at(1);
        let mantissa = if rest.is_empty() {
            first.into()
        } else {
            format!("{first}.{rest}")
        };
        return format!("{sign}{mantissa}e{exponent:+03}");
    }
    if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if point as usize >= digits.len() {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(point as usize - digits.len())
        )
    } else {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{sign}{whole}.{fraction}")
    }
}

pub fn python_value_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => python_number_str(n),
        Value::String(s) => python_repr(s),
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
                .map(|(k, v)| format!("{}: {}", python_repr(k), python_value_repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn python_str(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        value => python_value_repr(value),
    }
}

/// Render pre-rendered dictionary entries in the caller's insertion order.
/// Each value is already a Python repr, so nested ordered dictionaries can use
/// this function recursively without round-tripping through serde's sorted map.
pub fn python_dict_repr<'a>(items: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    format!(
        "{{{}}}",
        items
            .into_iter()
            .map(|(k, v)| format!("{}: {v}", python_repr(k)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
pub use python_str as python_string;
pub use python_truth as python_truthy;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_rendering_truth_and_exact_equality_match_python() {
        let mut values = vec![
            json!(null),
            json!(true),
            json!(false),
            json!(0),
            json!(1),
            json!(-1),
            json!(u64::MAX),
            json!(i64::MIN),
            json!(9007199254740993u64),
            json!(-0.0),
            json!(0.0),
            json!(1.0),
            json!(9007199254740992.0),
            json!(""),
            json!("é\u{1c}"),
            json!([]),
            json!({}),
            json!(["a'b", true, 1e-5]),
            json!({"a":"\u{200b}","b":[false,null]}),
        ];
        for exponent in -9..=20 {
            values.push(json!(10.0_f64.powi(exponent)));
        }
        let mut bits = 0x81e2_7635_a49b_c0df_u64;
        for _ in 0..512 {
            bits = bits.wrapping_mul(6364136223846793005).wrapping_add(1);
            if let Some(number) = serde_json::Number::from_f64(f64::from_bits(bits)) {
                values.push(Value::Number(number));
            }
        }
        let actual: Vec<_> = values
            .iter()
            .map(|v| json!([python_str(v), python_value_repr(v), python_truth(v)]))
            .collect();
        let Some(expected) = crate::test_support::python_values(
            "print(json.dumps([(str(v),repr(v),bool(v)) for v in values]))",
            &json!(values),
        ) else {
            return;
        };
        assert_eq!(json!(actual), expected);
        let mut equality = Vec::new();
        for a in &values[..19] {
            for b in &values[..19] {
                equality.push(json!([a, b]));
            }
        }
        equality.extend([
            json!([{"x":[true,0]}, {"x":[1.0,false]}]),
            json!([18446744073709551615u64, 18446744073709551616.0]),
            json!([-9223372036854775808i64, -9223372036854775808.0]),
        ]);
        let Some(expected) = crate::test_support::python_values(
            "print(json.dumps([a==b for a,b in values]))",
            &json!(equality),
        ) else {
            return;
        };
        let actual: Vec<_> = equality
            .iter()
            .map(|v| python_json_eq(&v[0], &v[1]))
            .collect();
        assert_eq!(json!(actual), expected);
    }
    #[test]
    fn whitespace_and_repr_unicode_classification() {
        for c in [
            '\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}', '\u{85}', '\u{a0}', '\u{3000}',
        ] {
            assert!(python_whitespace(c));
            assert_eq!(python_strip(&format!("{c}a{c}")), "a");
        }
        assert_eq!(
            python_dict_repr([("z", "True"), ("a", "{'y': None}")]),
            "{'z': True, 'a': {'y': None}}"
        );
        let Some(expected) = crate::test_support::python(
            "",
            r#"
import unicodedata
if unicodedata.unidata_version != '16.0.0':
    print('Unicode 16.0.0 oracle required for the pinned printable table',file=sys.stderr)
    sys.exit(77)
ranges=[]; start=None
for i in range(0x110000):
    no=not chr(i).isprintable() and not 0xd800<=i<=0xdfff
    if no and start is None: start=i
    if start is not None and (not no or i==0x10ffff):
        ranges.append([start,i-1 if not no else i]); start=None
print(json.dumps(ranges))
"#,
            &[],
        ) else {
            return;
        };
        let mut ranges = Vec::new();
        let mut start = None;
        for i in 0..=0x10ffff {
            let no = char::from_u32(i).is_some_and(|c| !unicode::printable(c));
            if no && start.is_none() {
                start = Some(i);
            }
            if let Some(s) = start {
                if !no || i == 0x10ffff {
                    ranges.push([s, if no { i } else { i - 1 }]);
                    start = None;
                }
            }
        }
        assert_eq!(json!(ranges), expected);
    }
    #[test]
    fn float_seconds_match_cpython_half_even_and_range() {
        let mut seconds = vec![
            0.0,
            0.0000005,
            -0.0000005,
            -0.0000015,
            1700000000.0000014,
            1735689600.7916253,
            -62135596800.0,
            -62135596801.0,
            253402300800.0,
        ];
        for i in 0..1024 {
            seconds.push(1735689600.0 + i as f64 / 7919.0);
        }
        let Some(expected) = crate::test_support::python_values(
            r#"
from datetime import datetime, UTC
out=[]
for v in values:
    try:
        d=datetime.fromtimestamp(v,UTC)-datetime(1970,1,1,tzinfo=UTC)
        out.append((d.days*86400+d.seconds)*1000000+d.microseconds)
    except (ValueError,OverflowError,OSError): out.append(None)
print(json.dumps(out))
"#,
            &json!(seconds),
        ) else {
            return;
        };
        let actual: Vec<_> = seconds
            .iter()
            .map(|&s| from_timestamp(s).ok().map(|t| t.as_microsecond()))
            .collect();
        assert_eq!(json!(actual), expected);
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
            assert!(from_timestamp(value).is_err());
        }
    }
    #[test]
    fn aware_adapter_defaults_to_pinned_python_and_checks_jiff_range() {
        let value = "2026-10-23T12:00:00-00:00:00.5";
        let pinned = python_fromisoformat(value).unwrap();
        assert_eq!(pinned.offset_microseconds, Some(0));
        assert_eq!(python_iso_timestamp(value), pinned.timestamp().ok());
        let newer = python_fromisoformat_with(value, IsoformatSemantics::Python3148).unwrap();
        assert_eq!(newer.offset_microseconds, Some(-500_000));
        assert_eq!(
            newer.timestamp().unwrap().as_microsecond()
                - pinned.timestamp().unwrap().as_microsecond(),
            500_000
        );
        assert!(python_iso_timestamp("2026-10-23T12:00:00").is_none());
        assert_eq!(
            python_fromisoformat("2026-10-23").unwrap().timestamp(),
            Err(DateTimeError::Naive)
        );
        assert!(from_timestamp(0.0).unwrap().to_jiff().is_ok());
        assert_eq!(
            python_iso_timestamp("9999-12-31T23:59:59.999999Z")
                .unwrap()
                .to_jiff(),
            Err(DateTimeError::TimeRange)
        );
    }

    #[test]
    fn iso_grammar_and_offsets_match_python() {
        let mut fixtures = vec![
            "".to_owned(),
            "bad".into(),
            "0000-01-01".into(),
            "2026-10-23T".into(),
            "2026-W54-1".into(),
            "2026-02-30".into(),
        ];
        for date in [
            "2026-10-23",
            "20261023",
            "2026-W43-5",
            "2026W435",
            "2026-W43",
            "2026W43",
            "0001-01-01",
            "9999-12-31",
        ] {
            fixtures.push(date.into());
            for separator in ["T", " ", "💩", "\n", "1"] {
                for clock in [
                    "12",
                    "1230",
                    "12:30",
                    "123000",
                    "12:30:00",
                    "12:30:00.12345699999",
                    "123000,12345699999",
                    "12.5",
                    "12:30.5",
                    "24:00",
                    "24:00:00.1",
                    "25",
                    "12:60",
                    "12:00:60",
                    "12:3000",
                ] {
                    for offset in [
                        "",
                        "Z",
                        "+01",
                        "-0130",
                        "+01:30",
                        "+01:02:03.1234569999",
                        "-000000.5",
                        "+00:00:00.5",
                        "+01.5",
                        "+01:02.5",
                        "+24:00",
                        "+00:99",
                        "+00:00:99",
                        "+0000Z",
                    ] {
                        fixtures.push(format!("{date}{separator}{clock}{offset}"));
                    }
                }
            }
        }
        let Some(expected) = crate::test_support::python_values(
            r#"
from datetime import datetime, UTC
if sys.version_info < (3,14):
    print('CPython 3.14 or newer required for ISO grammar oracle',file=sys.stderr)
    sys.exit(77)
out=[]
for s in values:
    try:
        dt=datetime.fromisoformat(s)
        if dt.tzinfo is None: out.append(['naive', dt.isoformat()]); continue
        d=dt.astimezone(UTC)-datetime(1970,1,1,tzinfo=UTC)
        out.append(['aware',(d.days*86400+d.seconds)*1000000+d.microseconds])
    except (ValueError,OverflowError): out.append(None)
probe=datetime.fromisoformat('2026-10-23T12:00:00+00:00:00.5').utcoffset().microseconds
print(json.dumps([probe,out]))
"#,
            &json!(fixtures),
        ) else {
            return;
        };
        let semantics = if expected[0] == 500000 {
            IsoformatSemantics::Python3148
        } else {
            assert_eq!(expected[0], 0, "unrecognised CPython offset semantics");
            IsoformatSemantics::Python3144
        };
        for (s, expected) in fixtures.iter().zip(expected[1].as_array().unwrap()) {
            let actual = match python_fromisoformat_with(s, semantics) {
                Ok(v) if v.offset_microseconds.is_none() => {
                    let d = v.datetime;
                    let mut text = format!(
                        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                        d.year(),
                        d.month(),
                        d.day(),
                        d.hour(),
                        d.minute(),
                        d.second()
                    );
                    if d.subsec_nanosecond() != 0 {
                        text.push_str(&format!(".{:06}", d.subsec_nanosecond() / 1000));
                    }
                    json!(["naive", text])
                }
                Ok(v) => v
                    .timestamp()
                    .ok()
                    .map(|t| json!(["aware", t.as_microsecond()]))
                    .unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            assert_eq!(&actual, expected, "{s:?}");
        }
    }
}

/// Python str.splitlines(False), including CRLF and its Unicode boundaries.
pub fn python_splitlines(text: &str) -> Vec<&str> {
    python_splitlines_with(text, false)
}
pub fn python_splitlines_with(text: &str, keep_ends: bool) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if !matches!(
            c,
            '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{1c}'
                ..='\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            continue;
        }
        let mut end = index + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|&(_, c)| c == '\n') {
            if let Some((i, c)) = chars.next() {
                end = i + c.len_utf8();
            }
        }
        lines.push(&text[start..if keep_ends { end } else { index }]);
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

#[cfg(test)]
mod line_tests {
    use super::*;
    #[test]
    fn splitlines_matches_python_empty_trailing_crlf_and_unicode_boundaries() {
        let mut values = vec!["".to_string(), "a\r\nb\r\n".into(), "\n\r\n\r".into()];
        for c in (0..=255)
            .filter_map(char::from_u32)
            .chain(['\u{2028}', '\u{2029}', '😀'])
        {
            values.push(format!("a{c}b{c}"));
        }
        let Some(expected) = crate::test_support::python_values(
            "print(json.dumps([[s.splitlines(),s.splitlines(True)] for s in values]))",
            &serde_json::json!(values),
        ) else {
            return;
        };
        let actual: Vec<_> = values
            .iter()
            .map(|v| serde_json::json!([python_splitlines(v), python_splitlines_with(v, true)]))
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
}
