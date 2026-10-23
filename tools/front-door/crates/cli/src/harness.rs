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
            .is_none_or(|s| provenant_core::py::python_strip(s).is_empty())
        {
            errors.push(format!("{prefix}: interface.{field} is required"));
        }
    }
    if let Some(short) = interface.get("short_description").and_then(Value::as_str) {
        if !provenant_core::py::python_strip(short).is_empty()
            && !(25..=64).contains(&short.chars().count())
        {
            errors.push(format!(
                "{prefix}: interface.short_description must be 25-64 characters"
            ));
        }
    }
    // Python str(container) uses repr for its members, including None/True.
    let prompt = interface
        .get("default_prompt")
        .map(provenant_core::py::python_str)
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

/// YAML decoding is supplied by the external-format owner. Preserve its diagnostic
/// verbatim, rather than mistaking invalid YAML for a missing interface mapping.
#[derive(Debug, thiserror::Error)]
#[error("{diagnostic}")]
pub struct YamlError {
    pub diagnostic: String,
}

pub fn validate_sidecar(
    display: &Path,
    skill: &str,
    document: Result<&Value, &YamlError>,
    asset_exists: impl Fn(&str) -> bool,
) -> Vec<String> {
    match document {
        Ok(document) => sidecar_errors(display, skill, document, asset_exists),
        Err(error) => vec![format!("{}: invalid YAML: {error}", display.display())],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sidecar_policy_and_invalid_yaml_match_the_source() {
        use std::fs;
        let Some(has_yaml): Option<bool> = crate::oracle::python(
            "import importlib.util,json; print(json.dumps(importlib.util.find_spec('yaml') is not None))",
            &json!(null),
        ) else {
            return;
        };
        if !has_yaml {
            eprintln!("skip: Python oracle lacks PyYAML (yaml)");
            assert_ne!(
                std::env::var("PROVENANT_PORT_DIFF_REQUIRED").as_deref(),
                Ok("1"),
                "required Python oracle dependency PyYAML unavailable"
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let scripts = dir.path().join("scripts");
        fs::create_dir(&scripts).unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../scripts/check_harness.py"),
            scripts.join("check_harness.py"),
        )
        .unwrap();
        let sidecar = dir.path().join("skills/build/agents/openai.yaml");
        fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        for document in [
            json!([]),
            json!({"interface":{}}),
            json!({"interface":{"display_name":"Build", "short_description":"x".repeat(25),"default_prompt":[true,null,"$build"],"icon_small":"./missing"}}),
        ] {
            fs::write(&sidecar, serde_json::to_vec(&document).unwrap()).unwrap();
            let Some(expected): Option<Vec<String>> = crate::oracle::python(
                r#"
import json,sys,importlib.util
from pathlib import Path
x=json.load(sys.stdin); p=Path(x['root'])/'scripts/check_harness.py'
spec=importlib.util.spec_from_file_location('harness_oracle',p); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
print(json.dumps(m.openai_sidecar_errors()))
"#,
                &json!({"root":dir.path()}),
            ) else {
                return;
            };
            assert_eq!(
                sidecar_errors(
                    Path::new("skills/build/agents/openai.yaml"),
                    "build",
                    &document,
                    |_| false
                ),
                expected
            );
        }
        fs::write(&sidecar, "interface: [\n").unwrap();
        let Some(expected): Option<Vec<String>> = crate::oracle::python(
            r#"
import json,sys,importlib.util
from pathlib import Path
x=json.load(sys.stdin); p=Path(x['root'])/'scripts/check_harness.py'
spec=importlib.util.spec_from_file_location('harness_oracle',p); m=importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
print(json.dumps(m.openai_sidecar_errors()))
"#,
            &json!({"root":dir.path()}),
        ) else {
            return;
        };
        let diagnostic = expected[0]
            .split_once(": invalid YAML: ")
            .unwrap()
            .1
            .to_owned();
        let error = YamlError { diagnostic };
        assert_eq!(
            validate_sidecar(
                Path::new("skills/build/agents/openai.yaml"),
                "build",
                Err(&error),
                |_| true
            ),
            expected
        );
    }

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
