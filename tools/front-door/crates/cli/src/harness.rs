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

/// Unit-02 orchestration seam: each implementation owns exactly the checker
/// named in the source; YAML/Markdown decoders can be integrated independently.
pub trait Checks {
    type Error;
    fn skills(&mut self) -> Result<Vec<String>, Self::Error>;
    fn markdown_links(&mut self, paths: &[std::path::PathBuf]) -> Result<Vec<String>, Self::Error>;
    fn issue_forms(&mut self, paths: &[std::path::PathBuf]) -> Result<Vec<String>, Self::Error>;
    fn sidecars(&mut self) -> Result<Vec<String>, Self::Error>;
}

fn markdown_paths(base: &Path, result: &mut Vec<std::path::PathBuf>) {
    // pathlib rglob suppresses directory-enumeration OSErrors. It does not
    // recurse into symlink directories, but yields a matching symlink itself.
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|s| s == "md") {
            result.push(path.clone());
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            markdown_paths(&path, result);
        }
    }
}
pub fn input_paths(root: &Path) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
    let mut docs = Vec::new();
    markdown_paths(&root.join("docs"), &mut docs);
    docs.sort();
    let mut forms = Vec::new();
    for base in [
        ".github/ISSUE_TEMPLATE",
        "skills/setup-repo/templates/ISSUE_TEMPLATE",
    ] {
        let mut paths = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root.join(base)) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|s| s == "yml")
                    && path.file_name().is_none_or(|s| s != "config.yml")
                {
                    paths.push(path);
                }
            }
        }
        paths.sort();
        forms.extend(paths);
    }
    (docs, forms)
}

pub fn render(errors: &[String]) -> crate::CommandOutput {
    if errors.is_empty() {
        crate::CommandOutput {
            stdout: "PASS: frontmatter, fixtures, links, issue forms and sidecars clean\n".into(),
            ..Default::default()
        }
    } else {
        crate::CommandOutput {
            stderr: errors.iter().map(|e| format!("FAIL: {e}\n")).collect(),
            exit_code: 1,
            ..Default::default()
        }
    }
}
pub fn run<C: Checks>(root: &Path, checks: &mut C) -> Result<crate::CommandOutput, C::Error> {
    let (docs, forms) = input_paths(root);
    let mut errors = checks.skills()?;
    errors.extend(checks.markdown_links(&docs)?);
    errors.extend(checks.issue_forms(&forms)?);
    errors.extend(checks.sidecars()?);
    Ok(render(&errors))
}

#[cfg(test)]
mod orchestration_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn path_selection_and_output_order_match_source_main_contract() {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "docs/b.md",
            "docs/nested/a.md",
            "docs/skip.txt",
            ".github/ISSUE_TEMPLATE/z.yml",
            ".github/ISSUE_TEMPLATE/config.yml",
            ".github/ISSUE_TEMPLATE/no.yaml",
            "skills/setup-repo/templates/ISSUE_TEMPLATE/a.yml",
        ] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let Some(expected):Option<(Vec<std::path::PathBuf>,Vec<std::path::PathBuf>)>=crate::oracle::python("import json,sys; from pathlib import Path; r=Path(json.load(sys.stdin)['root']); print(json.dumps([[str(p) for p in sorted((r/'docs').rglob('*.md'))],[str(p) for p in [*sorted((r/'.github/ISSUE_TEMPLATE').glob('*.yml')),*sorted((r/'skills/setup-repo/templates/ISSUE_TEMPLATE').glob('*.yml'))] if p.name!='config.yml']]))",&json!({"root":dir.path()})) else{return;};
        assert_eq!(input_paths(dir.path()), expected);
        struct Fixture;
        impl Checks for Fixture {
            type Error = std::convert::Infallible;
            fn skills(&mut self) -> Result<Vec<String>, Self::Error> {
                Ok(vec!["skills".into()])
            }
            fn markdown_links(
                &mut self,
                _: &[std::path::PathBuf],
            ) -> Result<Vec<String>, Self::Error> {
                Ok(vec!["links".into()])
            }
            fn issue_forms(
                &mut self,
                _: &[std::path::PathBuf],
            ) -> Result<Vec<String>, Self::Error> {
                Ok(vec!["forms".into()])
            }
            fn sidecars(&mut self) -> Result<Vec<String>, Self::Error> {
                Ok(vec!["sidecars".into()])
            }
        }
        assert_eq!(
            run(dir.path(), &mut Fixture).unwrap(),
            crate::CommandOutput {
                stderr: "FAIL: skills\nFAIL: links\nFAIL: forms\nFAIL: sidecars\n".into(),
                exit_code: 1,
                ..Default::default()
            }
        );
        assert_eq!(
            render(&[]).stdout,
            "PASS: frontmatter, fixtures, links, issue forms and sidecars clean\n"
        );
    }
}
