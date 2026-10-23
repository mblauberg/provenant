//! Temporary shared types until their owning crates expose the accepted API.

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
/// Temporary finite JSON reader. Python non-finite values, lone surrogates and
/// unbounded integer semantics are still blocked; see RP-1001 and PORT_STATUS.
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
    let value: Value = serde_json::from_slice(&bytes).map_err(|source| PortableError::Json {
        path: path.into(),
        source,
    })?;
    value.as_object().cloned().ok_or_else(|| {
        PortableError::Invalid(format!("{} must contain one JSON object", path.display()))
    })
}
