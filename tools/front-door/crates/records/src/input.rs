//! Diagnostics for open JSON request objects.
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InputWarning {
    pub source: String,
    pub key: String,
}

impl std::fmt::Display for InputWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}: unknown key ignored", self.source, self.key)
    }
}

/// Decode an agent request and report keys the request type discarded.
/// Request wire types must serialise every recognised input field, including
/// absent optional values. Records with flattened extension maps retain those
/// fields and therefore do not produce ignored-key diagnostics.
pub fn decode_input<T: DeserializeOwned + Serialize>(
    source: &str,
    bytes: &[u8],
) -> Result<(T, Vec<InputWarning>), serde_json::Error> {
    // Decode the typed input first, preserving duplicate-known-field and type
    // errors from serde. Parsing a Value first would discard duplicate fields.
    let input: T = serde_json::from_slice(bytes)?;
    let original: Value = serde_json::from_slice(bytes)?;
    let accepted = serde_json::to_value(&input)?;
    let mut warnings = Vec::new();
    discarded(source, "", &original, &accepted, &mut warnings);
    Ok((input, warnings))
}

fn discarded(
    source: &str,
    prefix: &str,
    original: &Value,
    accepted: &Value,
    warnings: &mut Vec<InputWarning>,
) {
    match (original, accepted) {
        (Value::Object(original), Value::Object(accepted)) => {
            for (key, value) in original {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                match accepted.get(key) {
                    Some(accepted) => discarded(source, &path, value, accepted, warnings),
                    None => warnings.push(InputWarning {
                        source: source.to_owned(),
                        key: path,
                    }),
                }
            }
        }
        (Value::Array(original), Value::Array(accepted)) => {
            for (index, (value, accepted)) in original.iter().zip(accepted).enumerate() {
                discarded(
                    source,
                    &format!("{prefix}[{index}]"),
                    value,
                    accepted,
                    warnings,
                );
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize, Serialize)]
    struct Input {
        rows: Vec<Row>,
        optional: Option<String>,
    }
    #[derive(Deserialize, Serialize)]
    struct Row {
        name: String,
    }

    #[test]
    fn unknown_keys_report_the_input_file_and_nested_key() {
        let (input, warnings) = decode_input::<Input>(
            "request.json",
            br#"{"rows":[{"name":"kept","future":true}],"optional":null,"typo":1}"#,
        )
        .unwrap();
        assert_eq!(input.rows[0].name, "kept");
        assert_eq!(
            warnings.iter().map(|w| w.key.as_str()).collect::<Vec<_>>(),
            ["rows[0].future", "typo"]
        );
        assert!(warnings.iter().all(|w| w.source == "request.json"));
        assert_eq!(
            warnings[0].to_string(),
            "request.json: rows[0].future: unknown key ignored"
        );
    }

    #[test]
    fn syntax_duplicate_fields_and_invalid_types_remain_errors() {
        for bytes in [
            &br#"{"rows":[],"rows":[]}"#[..],
            &br#"{"rows":false}"#[..],
            &br#"{"rows":[]"#[..],
        ] {
            assert!(decode_input::<Input>("request.json", bytes).is_err());
        }
    }
}
