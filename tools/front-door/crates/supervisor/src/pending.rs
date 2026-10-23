//! Fixed-record encoders awaiting the records Python JSON owner.

// port: belongs in provenant-records
/// Python json.dumps(ensure_ascii=True) string spelling at this fixed record edge.
pub(crate) fn json_ascii(text: &str) -> String {
    let mut out = String::from("\"");
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            ch if !(' '..='~').contains(&ch) => {
                for unit in ch.encode_utf16(&mut [0; 2]) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

// port: belongs in provenant-records
/// JSON permits CPython's non-finite extension; finite float spelling is core's.
pub(crate) fn json_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value == f64::INFINITY {
        return "Infinity".into();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".into();
    }
    provenant_core::py::python_str(&serde_json::Value::from(value))
}
