//! Compatibility helpers for the source validators' JSON type boundaries.

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("cannot use '{kind}' as a set element (unhashable type: '{kind}')")]
    Unhashable {
        field: &'static str,
        kind: &'static str,
    },
    #[error("Python integer is outside the i64/u64 range")]
    NumberOutOfRange,
    #[error("invalid review JSON: {0}")]
    InvalidJson(String),
}

pub(crate) fn hashable(value: &serde_json::Value, field: &'static str) -> Result<(), PolicyError> {
    let kind = match value {
        serde_json::Value::Array(_) => "list",
        serde_json::Value::Object(_) => "dict",
        _ => return Ok(()),
    };
    Err(PolicyError::Unhashable { field, kind })
}

pub(crate) fn strip(value: &str) -> &str {
    provenant_core::py::python_strip(value)
}
pub(crate) fn whitespace(c: char) -> bool {
    provenant_core::py::python_whitespace(c)
}
