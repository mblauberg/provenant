//! Private compatibility primitives awaiting their owning crate.
mod python_unicode;

// port: belongs in provenant-core
// Unicode decimal digits, as accepted by Python re \d and int.
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
pub(crate) fn python_strip(text: &str) -> &str {
    text.trim_matches(python_whitespace)
}

// port: belongs in provenant-core
pub(crate) fn python_whitespace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}
// port: belongs in provenant-core
pub(crate) fn python_alpha(text: &str) -> bool {
    // regex-syntax 0.8.11 and the copied CLI helpers use Unicode 16.0.0.
    // Python isalpha uses General_Category Letter, not Rust's Alphabetic property.
    static LETTERS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\A\p{L}+\z").expect("Unicode letter regex")
    });
    LETTERS.is_match(text)
}

// port: belongs in provenant-core
pub(crate) fn python_truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(v) => *v,
        serde_json::Value::String(v) => !v.is_empty(),
        serde_json::Value::Number(v) => v.as_f64() != Some(0.0),
        serde_json::Value::Array(v) => !v.is_empty(),
        serde_json::Value::Object(v) => !v.is_empty(),
    }
}

// port: belongs in provenant-core
pub(crate) fn python_str(value: &serde_json::Value) -> String {
    if let serde_json::Value::String(text) = value {
        return text.clone();
    }
    python_value_repr(value)
}
// port: belongs in provenant-core
fn python_value_repr(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(text) => python_repr(text),
        Value::Number(_) => super::python_json(value),
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
                .map(|(key, value)| format!("{}: {}", python_repr(key), python_value_repr(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
// port: belongs in provenant-core
pub(crate) fn python_equal(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    use serde_json::Value;
    let number = |value: &Value| match value {
        Value::Number(n) => Some(crate::Numeric::from(n.clone())),
        Value::Bool(b) => Some(crate::Numeric::from(i32::from(*b))),
        _ => None,
    };
    if let (Some(left), Some(right)) = (number(left), number(right)) {
        return left == right;
    }
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| python_equal(a, b))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|other| python_equal(value, other))
                })
        }
        _ => left == right,
    }
}
