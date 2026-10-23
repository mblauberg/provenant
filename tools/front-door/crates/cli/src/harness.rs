//! Pure sidecar policy. YAML decoding and harness traversal await their owners.
use serde_json::Value;
use std::path::Path;

/// Validate decoded YAML values without conflating malformed fields with missing ones.
/// `asset_exists` is an observation from the configuration/filesystem owner.
pub fn sidecar_errors(
    display: &Path,
    skill: &str,
    document: &Value,
    asset_exists: impl Fn(&str) -> bool,
) -> Vec<String> {
    let prefix = display.display();
    let Some(interface) = document.get("interface").and_then(Value::as_object) else {
        return vec![format!("{prefix}: interface mapping is required")];
    };
    let mut errors = Vec::new();
    for field in ["display_name", "short_description", "default_prompt"] {
        if interface
            .get(field)
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
        {
            errors.push(format!("{prefix}: interface.{field} is required"));
        }
    }
    if let Some(short) = interface.get("short_description").and_then(Value::as_str) {
        if !short.trim().is_empty() && !(25..=64).contains(&short.chars().count()) {
            errors.push(format!(
                "{prefix}: interface.short_description must be 25-64 characters"
            ));
        }
    }
    // For non-string prompts Python stringifies the value. A JSON value cannot
    // contain '$skill' in scalar non-string values; collections may contain it.
    let prompt = interface
        .get("default_prompt")
        .map(Value::to_string)
        .unwrap_or_default();
    if !prompt.contains(&format!("${skill}")) {
        errors.push(format!("{prefix}: default_prompt must invoke ${skill}"));
    }
    for icon in ["icon_small", "icon_large"] {
        if let Some(value) = interface.get(icon).and_then(Value::as_str) {
            if value.starts_with("./") && !asset_exists(value) {
                errors.push(format!("{prefix}: missing {icon} asset {value}"));
            }
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requires_mapping_and_reports_fields_in_source_order() {
        assert_eq!(
            sidecar_errors(Path::new("s.yaml"), "build", &json!([]), |_| true),
            ["s.yaml: interface mapping is required"]
        );
        assert_eq!(
            sidecar_errors(
                Path::new("s.yaml"),
                "build",
                &json!({"interface":{}}),
                |_| true
            ),
            [
                "s.yaml: interface.display_name is required",
                "s.yaml: interface.short_description is required",
                "s.yaml: interface.default_prompt is required",
                "s.yaml: default_prompt must invoke $build",
            ]
        );
    }

    #[test]
    fn counts_unicode_characters_and_checks_only_relative_icons() {
        for length in [24, 25, 64, 65] {
            let doc = json!({"interface": {
                "display_name": "Build", "short_description": "é".repeat(length),
                "default_prompt": "Use $build", "icon_small": "./missing.svg", "icon_large": "https://image"
            }});
            let errors = sidecar_errors(Path::new("s.yaml"), "build", &doc, |_| false);
            assert_eq!(errors.len(), 1 + usize::from(!(25..=64).contains(&length)));
            assert_eq!(
                errors.last().unwrap(),
                "s.yaml: missing icon_small asset ./missing.svg"
            );
        }
    }
}
