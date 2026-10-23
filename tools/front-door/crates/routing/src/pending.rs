//! Minimal bridges awaiting ordered ingress / Unicode APIs from shared owners.

pub(crate) use provenant_core::py::{python_repr, python_string, python_strip};
pub use provenant_records::checked_json::DecodeError as JsonError;

/// Checked range and grammar are owned by records. The second pass retains map
/// order and duplicate-member evidence, which its Value API cannot represent.
// port: belongs in provenant-records (ordered generic decoder)
pub fn from_json<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, JsonError> {
    provenant_records::checked_json::from_str(text)?;
    let mut output = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        output.push(c);
        if c == '"' {
            while let Some(c) = chars.next() {
                output.push(c);
                if c == '\\' {
                    if let Some(c) = chars.next() {
                        output.push(c);
                    }
                } else if c == '"' {
                    break;
                }
            }
        } else if c == '-' || c.is_ascii_digit() {
            let mut token = String::from(c);
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'))
            {
                if let Some(c) = chars.next() {
                    token.push(c);
                }
            }
            output.pop();
            output.push_str(if token == "-0" { "0" } else { &token });
        }
    }
    Ok(serde_json::from_str(&output)?)
}

pub(crate) fn value_truthy(value: &serde_json::Value) -> bool {
    provenant_core::py::python_truth(value)
}
pub(crate) fn truthy(value: &crate::ordered_json::OrderedValue) -> bool {
    provenant_core::py::python_truth(&value.to_value())
}
pub(crate) fn python_one(value: Option<&serde_json::Value>) -> bool {
    value.is_some_and(|v| provenant_core::py::python_json_eq(v, &serde_json::Value::from(1)))
}
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
        OrderedValue::Object(items) => {
            let rendered: Vec<_> = items.0.iter().map(|(k, v)| (k.as_str(), repr(v))).collect();
            provenant_core::py::python_dict_repr(rendered.iter().map(|(k, v)| (*k, v.as_str())))
        }
        _ => python_string(&value.to_value()),
    }
}

/// Python json.dumps encoding; shared records owner has no ordered text encoder.
// port: belongs in provenant-records
pub(crate) fn json_dumps(
    value: &crate::ordered_json::OrderedValue,
    sort_keys: bool,
    indent: Option<usize>,
) -> String {
    use crate::ordered_json::OrderedValue;
    fn string(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if !(' '..='~').contains(&c) => {
                    let mut buf = [0; 2];
                    for n in c.encode_utf16(&mut buf) {
                        out.push_str(&format!("\\u{n:04x}"));
                    }
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
    fn encode(v: &OrderedValue, sort: bool, indent: Option<usize>, depth: usize) -> String {
        let (open, close, values) = match v {
            OrderedValue::Null => return "null".into(),
            OrderedValue::Bool(b) => return b.to_string(),
            OrderedValue::Number(n) => {
                return provenant_core::py::python_str(&serde_json::Value::Number(n.clone()))
            }
            OrderedValue::String(s) => return string(s),
            OrderedValue::Array(a) => (
                '[',
                ']',
                a.iter()
                    .map(|v| encode(v, sort, indent, depth + 1))
                    .collect::<Vec<_>>(),
            ),
            OrderedValue::Object(o) => {
                let mut entries: Vec<_> = o.0.iter().collect();
                if sort {
                    entries.sort_by(|a, b| a.0.cmp(&b.0));
                }
                (
                    '{',
                    '}',
                    entries
                        .into_iter()
                        .map(|(k, v)| {
                            format!("{}: {}", string(k), encode(v, sort, indent, depth + 1))
                        })
                        .collect(),
                )
            }
        };
        if values.is_empty() {
            return format!("{open}{close}");
        }
        match indent {
            None => format!("{open}{}{close}", values.join(", ")),
            Some(n) => {
                let pad = " ".repeat(n.saturating_mul(depth + 1));
                format!(
                    "{open}\n{pad}{}\n{}{close}",
                    values.join(&format!(",\n{pad}")),
                    " ".repeat(n.saturating_mul(depth))
                )
            }
        }
    }
    encode(value, sort_keys, indent, 0)
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
                Err(JsonError::NumberOutOfRange(_))
            ));
        }
        let text = r#"{"integer-looking text":"18446744073709551616","float":1.0}"#;
        assert_eq!(
            serde_json::to_string(&from_json::<OrderedValue>(text).unwrap()).unwrap(),
            text
        );
    }
}
