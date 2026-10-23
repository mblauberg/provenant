//! Compatibility helpers for the source validators' JSON type boundaries.

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("cannot use '{kind}' as a set element (unhashable type: '{kind}')")]
    Unhashable {
        field: &'static str,
        kind: &'static str,
    },
}

pub(crate) fn hashable(value: &serde_json::Value, field: &'static str) -> Result<(), PolicyError> {
    let kind = match value {
        serde_json::Value::Array(_) => "list",
        serde_json::Value::Object(_) => "dict",
        _ => return Ok(()),
    };
    Err(PolicyError::Unhashable { field, kind })
}

pub(crate) fn whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}
pub(crate) fn strip(value: &str) -> &str {
    value.trim_matches(whitespace)
}
