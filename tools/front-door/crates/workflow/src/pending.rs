//! Temporary shared types and Python compatibility helpers awaiting their owners.

mod python_unicode;

// port: belongs in provenant-core
// Unicode decimal digits, as accepted by Python re \d and int.
#[allow(dead_code)]
pub(crate) fn decimal_digit(c: char) -> Option<u8> {
    let code = u32::from(c);
    // port: belongs in provenant-core
    const ZEROS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450,
        0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50,
        0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce,
        0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
    ];
    ZEROS
        .iter()
        .find_map(|&zero| code.checked_sub(zero).filter(|&d| d < 10).map(|d| d as u8))
}

// port: belongs in provenant-core
#[allow(dead_code)]
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
            c if !python_unicode::printable(c) => {
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
#[allow(dead_code)]
pub(crate) fn python_digit(c: char) -> bool {
    decimal_digit(c).is_some()
        || matches!(
            u32::from(c),
            0xb2 | 0xb3
                | 0xb9
                | 0x1369
                | 0x136a
                | 0x136b
                | 0x136c
                | 0x136d
                | 0x136e
                | 0x136f
                | 0x1370
                | 0x1371
                | 0x19da
                | 0x2070
                | 0x2074
                | 0x2075
                | 0x2076
                | 0x2077
                | 0x2078
                | 0x2079
                | 0x2080
                | 0x2081
                | 0x2082
                | 0x2083
                | 0x2084
                | 0x2085
                | 0x2086
                | 0x2087
                | 0x2088
                | 0x2089
                | 0x2460
                | 0x2461
                | 0x2462
                | 0x2463
                | 0x2464
                | 0x2465
                | 0x2466
                | 0x2467
                | 0x2468
                | 0x2474
                | 0x2475
                | 0x2476
                | 0x2477
                | 0x2478
                | 0x2479
                | 0x247a
                | 0x247b
                | 0x247c
                | 0x2488
                | 0x2489
                | 0x248a
                | 0x248b
                | 0x248c
                | 0x248d
                | 0x248e
                | 0x248f
                | 0x2490
                | 0x24ea
                | 0x24f5
                | 0x24f6
                | 0x24f7
                | 0x24f8
                | 0x24f9
                | 0x24fa
                | 0x24fb
                | 0x24fc
                | 0x24fd
                | 0x24ff
                | 0x2776
                | 0x2777
                | 0x2778
                | 0x2779
                | 0x277a
                | 0x277b
                | 0x277c
                | 0x277d
                | 0x277e
                | 0x2780
                | 0x2781
                | 0x2782
                | 0x2783
                | 0x2784
                | 0x2785
                | 0x2786
                | 0x2787
                | 0x2788
                | 0x278a
                | 0x278b
                | 0x278c
                | 0x278d
                | 0x278e
                | 0x278f
                | 0x2790
                | 0x2791
                | 0x2792
                | 0x10a40
                | 0x10a41
                | 0x10a42
                | 0x10a43
                | 0x10e60
                | 0x10e61
                | 0x10e62
                | 0x10e63
                | 0x10e64
                | 0x10e65
                | 0x10e66
                | 0x10e67
                | 0x10e68
                | 0x11052
                | 0x11053
                | 0x11054
                | 0x11055
                | 0x11056
                | 0x11057
                | 0x11058
                | 0x11059
                | 0x1105a
                | 0x1f100
                | 0x1f101
                | 0x1f102
                | 0x1f103
                | 0x1f104
                | 0x1f105
                | 0x1f106
                | 0x1f107
                | 0x1f108
                | 0x1f109
                | 0x1f10a
        )
}

// port: belongs in provenant-core
#[allow(dead_code)]
pub(crate) fn python_whitespace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}

// port: belongs in provenant-core
pub(crate) fn python_strip(text: &str) -> &str {
    text.trim_matches(python_whitespace)
}

// port: belongs in provenant-confinement
#[derive(Debug, thiserror::Error)]
pub enum OwnedFileError {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    MultipleLinks(String),
    #[error("{message}")]
    Io {
        message: String,
        #[source]
        source: std::io::Error,
    },
}

// port: belongs in provenant-records
/// Python json.dumps(sort_keys=True), for string/integer artifact records.
pub(crate) fn python_json(value: &serde_json::Value) -> String {
    use serde_json::Value;
    fn string(s: &str) -> String {
        let mut result = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => result.push_str("\\\""),
                '\\' => result.push_str("\\\\"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                '\u{8}' => result.push_str("\\b"),
                '\u{c}' => result.push_str("\\f"),
                c if !(' '..='~').contains(&c) => {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        result.push_str(&format!("\\u{unit:04x}"));
                    }
                }
                c => result.push(c),
            }
        }
        result.push('"');
        result
    }
    match value {
        Value::String(s) => string(s),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", string(k), python_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.to_string(),
    }
}

// port: belongs in provenant-config
fn contains_out_of_range_integer(bytes: &[u8]) -> bool {
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            match (byte, escaped) {
                (_, true) => escaped = false,
                (b'\\', false) => escaped = true,
                (b'"', false) => in_string = false,
                _ => {}
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            index += 1;
            continue;
        }
        if byte == b'-' || byte.is_ascii_digit() {
            let start = index;
            while index < bytes.len()
                && !matches!(
                    bytes[index],
                    b',' | b']' | b'}' | b' ' | b'\t' | b'\r' | b'\n'
                )
            {
                index += 1;
            }
            let token = &bytes[start..index];
            let digits = token.strip_prefix(b"-").unwrap_or(token);
            let is_integer = !digits.is_empty()
                && digits.iter().all(u8::is_ascii_digit)
                && (digits == b"0" || digits.first() != Some(&b'0'));
            if is_integer
                && std::str::from_utf8(token).is_ok_and(|number| {
                    number.parse::<i64>().is_err() && number.parse::<u64>().is_err()
                })
            {
                return true;
            }
            continue;
        }
        index += 1;
    }
    false
}

pub(crate) enum PythonJsonError {
    NumberOutOfRange,
    Invalid(serde_json::Error),
}

pub(crate) fn parse_python_json(bytes: &[u8]) -> Result<serde_json::Value, PythonJsonError> {
    if contains_out_of_range_integer(bytes) {
        return Err(PythonJsonError::NumberOutOfRange);
    }
    serde_json::from_slice(bytes).map_err(PythonJsonError::Invalid)
}

/// Temporary finite JSON reader. Non-finite values and lone surrogates remain
/// parser integration gaps; integer overflow uses typed NumberOutOfRange.
pub(crate) fn read_python_object(
    path: &std::path::Path,
) -> Result<serde_json::Map<String, serde_json::Value>, crate::portable_workflow::PortableError> {
    use crate::portable_workflow::PortableError;
    use serde_json::Value;
    use std::fs;

    let bytes = fs::read(path).map_err(|source| PortableError::Io {
        path: path.into(),
        source,
    })?;
    let value: Value = parse_python_json(&bytes).map_err(|error| match error {
        PythonJsonError::NumberOutOfRange => PortableError::NumberOutOfRange { path: path.into() },
        PythonJsonError::Invalid(source) => PortableError::Json {
            path: path.into(),
            source,
        },
    })?;
    value.as_object().cloned().ok_or_else(|| {
        PortableError::Invalid(format!("{} must contain one JSON object", path.display()))
    })
}

// port: belongs in provenant-core
pub(crate) fn equals_one(value: &serde_json::Value) -> bool {
    use serde_json::Value;
    match value {
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_i64() == Some(1) || (number.is_f64() && number.as_f64() == Some(1.0)),
        _ => false,
    }
}

// port: belongs in provenant-core
pub(crate) fn splitlines(text: &str) -> Vec<&str> {
    let mut result=Vec::new(); let mut start=0; let mut chars=text.char_indices().peekable();
    while let Some((i,c))=chars.next() {
        if matches!(c,'\n'|'\r'|'\u{b}'|'\u{c}'|'\u{1c}'|'\u{1d}'|'\u{1e}'|'\u{85}'|'\u{2028}'|'\u{2029}') {
            result.push(&text[start..i]); start=i+c.len_utf8();
            if c=='\r' && chars.peek().is_some_and(|(_,c)|*c=='\n') {chars.next();start+=1;}
        }
    }
    if start<text.len() {result.push(&text[start..]);}
    result
}

// port: belongs in provenant-core
/// Exact JSON-domain Python equality, including bool/int/float comparisons.
pub(crate) fn python_json_equal(left:&serde_json::Value,right:&serde_json::Value)->bool {
    use serde_json::Value;
    if let Some(integer)=python_integer_value(left) {
        if let Some(other)=self::python_integer_value(right) {return integer==other;}
        if let Some(float)=right.as_number().filter(|n|n.is_f64()).and_then(|n|n.as_f64()) {
            return float.fract()==0.0 && (-9223372036854775808.0..18446744073709551616.0).contains(&float) && float as i128==integer;
        }
        return false;
    }
    if python_integer_value(right).is_some() {return python_json_equal(right,left);}
    match (left,right) {
        (Value::Array(a),Value::Array(b))=>a.len()==b.len() && a.iter().zip(b).all(|(a,b)|python_json_equal(a,b)),
        (Value::Object(a),Value::Object(b))=>a.len()==b.len() && a.iter().all(|(k,v)|b.get(k).is_some_and(|b|python_json_equal(v,b))),
        _=>left==right,
    }
}

// port: belongs in provenant-core
fn python_integer_value(value:&serde_json::Value)->Option<i128> {
    match value {
        serde_json::Value::Bool(b)=>Some(i128::from(*b)),
        serde_json::Value::Number(n) if !n.is_f64()=>n.as_i64().map(i128::from).or_else(||n.as_u64().map(i128::from)),
        _=>None,
    }
}

mod python_word;

// port: belongs in provenant-core
pub(crate) fn python_word(c:char)->bool {python_word::is_word(c)}
